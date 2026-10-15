"""PPO on a SimForge scenario set: ``python -m simforge_rl.examples.ppo --tasks richmond-field-station --out DIR``."""

from __future__ import annotations

import argparse
import csv
import hashlib
import importlib.metadata
import json
import os
import platform
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

import numpy as np

from simforge_rl import RuntimeSessions, Scenario, ScenarioServers, ScenarioSet, ScenarioSetEnv, bundled_sets, make_vector_env

OBJECT_ROWS = 8
SPEED_MAX_MPS = 30.0
ACCEL_MAX_MPS2 = 3.0


def features(obs: dict[str, np.ndarray]) -> np.ndarray:
    """Pose-free ego state plus the nearest object rows, batched."""
    state = np.asarray(obs["state_vector"], dtype=np.float64)[..., 2:]
    objects = np.asarray(obs["objects"], dtype=np.float64)[..., :OBJECT_ROWS, :]
    return np.concatenate([state, objects.reshape(*objects.shape[:-2], -1)], axis=-1).astype(np.float32)


def to_setpoint(action: np.ndarray) -> np.ndarray:
    a = np.clip(action, -1.0, 1.0)
    return np.stack([(a[..., 0] + 1.0) * 0.5 * SPEED_MAX_MPS, a[..., 1] * ACCEL_MAX_MPS2], axis=-1).astype(np.float64)


class RunningNorm:
    def __init__(self, size: int) -> None:
        self.mean, self.var, self.count = np.zeros(size), np.ones(size), 1e-4

    def update(self, x: np.ndarray) -> None:
        batch_mean, batch_var, n = x.mean(0), x.var(0), x.shape[0]
        delta, total = batch_mean - self.mean, self.count + n
        self.mean = self.mean + delta * n / total
        self.var = (self.var * self.count + batch_var * n + delta**2 * self.count * n / total) / total
        self.count = total

    def __call__(self, x: np.ndarray) -> np.ndarray:
        return np.clip((x - self.mean) / np.sqrt(self.var + 1e-8), -10.0, 10.0).astype(np.float32)


def build_policy(torch: Any, obs_size: int) -> Any:
    nn = torch.nn

    class ActorCritic(nn.Module):
        def __init__(self) -> None:
            super().__init__()
            self.actor = nn.Sequential(nn.Linear(obs_size, 64), nn.Tanh(), nn.Linear(64, 64), nn.Tanh(), nn.Linear(64, 2))
            self.critic = nn.Sequential(nn.Linear(obs_size, 64), nn.Tanh(), nn.Linear(64, 64), nn.Tanh(), nn.Linear(64, 1))
            self.log_std = nn.Parameter(torch.full((2,), -0.5))
            nn.init.orthogonal_(self.actor[-1].weight, 0.01)
            nn.init.zeros_(self.actor[-1].bias)

        def dist(self, x: Any) -> Any:
            return torch.distributions.Normal(self.actor(x), self.log_std.exp())

    return ActorCritic()


def evaluate(torch: Any, policy: Any, norm: RunningNorm, envs: dict[str, Any], tasks: list[tuple[str, int]]) -> dict[str, float]:
    """Deterministic (mean-action) return per held-out (scenario, seed) task."""
    returns: dict[str, list[float]] = {}
    for name, seed in tasks:
        env = envs[name]
        obs, _ = env.reset(options={"scenario_name": name, "episode_seed": seed})
        total, done = 0.0, False
        while not done:
            with torch.no_grad():
                mean = policy.actor(torch.as_tensor(norm(features(obs)[None])))[0].numpy()
            obs, reward, terminated, truncated, _ = env.step(to_setpoint(mean))
            total += reward
            done = terminated or truncated
        returns.setdefault(name, []).append(total)
    return {name: float(np.mean(values)) for name, values in returns.items()}


def authored_baseline(envs: dict[str, Any], tasks: list[tuple[str, int]]) -> dict[str, float]:
    """The scenario's own authored ego control (``action=None``) on the same tasks."""
    returns: dict[str, list[float]] = {}
    for name, seed in tasks:
        env = envs[name]
        env.reset(options={"scenario_name": name, "episode_seed": seed})
        total, done = 0.0, False
        while not done:
            _, reward, terminated, truncated, _ = env.step(None)
            total += reward
            done = terminated or truncated
        returns.setdefault(name, []).append(total)
    return {name: float(np.mean(values)) for name, values in returns.items()}


def svg_curve(rows: list[dict[str, Any]], baseline: float, path: Path) -> None:
    """A dependency-free SVG of eval and training return against environment steps."""
    w, h, pad = 720, 420, 56
    xs = [r["env_steps"] for r in rows]
    series = {"eval_return": "#1f77b4", "train_return": "#ff7f0e"}
    ys = [r[k] for r in rows for k in series if r[k] is not None] + [baseline]
    x0, x1, y0, y1 = 0, max(xs) or 1, min(ys), max(ys)
    y0, y1 = y0 - 0.05 * (y1 - y0 or 1), y1 + 0.05 * (y1 - y0 or 1)
    sx = lambda x: pad + (x - x0) / (x1 - x0) * (w - 2 * pad)
    sy = lambda y: h - pad - (y - y0) / (y1 - y0) * (h - 2 * pad)
    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" font-family="sans-serif" font-size="12">',
             f'<rect width="{w}" height="{h}" fill="white"/>',
             f'<line x1="{pad}" y1="{h - pad}" x2="{w - pad}" y2="{h - pad}" stroke="black"/>',
             f'<line x1="{pad}" y1="{pad}" x2="{pad}" y2="{h - pad}" stroke="black"/>',
             f'<text x="{w / 2}" y="{h - 14}" text-anchor="middle">environment steps (max {x1})</text>',
             f'<text x="14" y="{h / 2}" transform="rotate(-90 14 {h / 2})" text-anchor="middle">mean episode return</text>',
             f'<text x="{pad - 6}" y="{sy(y0) + 4:.1f}" text-anchor="end">{y0:.1f}</text>',
             f'<text x="{pad - 6}" y="{sy(y1) + 4:.1f}" text-anchor="end">{y1:.1f}</text>',
             f'<line x1="{pad}" y1="{sy(baseline):.1f}" x2="{w - pad}" y2="{sy(baseline):.1f}" stroke="#888" stroke-dasharray="4 4"/>']
    for i, (key, color) in enumerate(series.items()):
        points = " ".join(f"{sx(r['env_steps']):.1f},{sy(r[key]):.1f}" for r in rows if r[key] is not None)
        parts.append(f'<polyline fill="none" stroke="{color}" stroke-width="2" points="{points}"/>')
        parts.append(f'<text x="{w - pad - 150}" y="{pad + 16 * i}" fill="{color}">{key}</text>')
    parts.append(f'<text x="{w - pad - 150}" y="{pad + 32}" fill="#888">authored control</text></svg>')
    path.write_text("\n".join(parts) + "\n")


def version(dist: str) -> str | None:
    try:
        return importlib.metadata.version(dist)
    except importlib.metadata.PackageNotFoundError:
        return None


def parse_pairs(values: list[str], flag: str) -> dict[str, str]:
    pairs = {}
    for value in values:
        name, sep, rest = value.partition("=")
        if not sep or not name or not rest:
            raise SystemExit(f"{flag} expects NAME=VALUE, got {value!r}")
        pairs[name] = rest
    return pairs


def scenario_set_from(args: Any) -> ScenarioSet:
    """``--scenario-set``, ``--endpoint`` or ``--export`` if given (at most one), else the bundled ``--tasks`` set."""
    given = [flag for flag, value in (("--scenario-set", args.scenario_set), ("--endpoint", args.endpoint), ("--export", args.export)) if value]
    if len(given) > 1:
        raise SystemExit(f"pass one task source, not {' and '.join(given)}")
    if args.scenario_set:
        return ScenarioSet.load(args.scenario_set)
    if args.endpoint:
        scenarios = []
        for i, value in enumerate(args.endpoint):
            name, sep, path = value.partition("=")
            scenarios.append(Scenario(name, endpoint=path) if sep else Scenario(f"endpoint-{i}", endpoint=value))
        return ScenarioSet.of(scenarios)
    if args.export:
        return ScenarioSet.from_export(args.export, split=args.split, map_name=args.export_map, binary=args.binary)
    if args.tasks not in bundled_sets():
        raise SystemExit(f"--tasks {args.tasks!r} is not a bundled set; bundled: {', '.join(bundled_sets())}")
    return ScenarioSet.bundled(args.tasks)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m simforge_rl.examples.ppo", description=__doc__)
    parser.add_argument("--tasks", default="richmond-field-station", help=f"bundled task set, one per released map: {', '.join(bundled_sets())}")
    parser.add_argument("--endpoint", action="append", default=[], metavar="[NAME=]PATH", help="running `simforge env serve --socket` endpoint (repeatable)")
    parser.add_argument("--scenario-set", metavar="FILE", help="simforge-rl.scenario-set/v1 file (ScenarioSet.save)")
    parser.add_argument("--export", metavar="DIR|EXPORT_ID", help="train on a dataset export: a downloaded folder or a cloud export id")
    parser.add_argument("--split", default="train", help="the export split to train on")
    parser.add_argument("--export-map", metavar="NAME", help="only the export's scenarios on this map")
    parser.add_argument("--map-dir", help="map directory for template scenarios (default: the set's pinned installed map)")
    parser.add_argument("--runtime", nargs="?", const="", metavar="BIN", help="open sessions through `simforge-runtime serve` stdio (no BIN: the installed runtime) instead of env serve sockets")
    parser.add_argument("--max-open", type=int, help="sessions each sub-env keeps open (default: one per scenario); for large exports")
    parser.add_argument("--eval-limit", type=int, help="evaluate on the first N scenarios only (default: all)")
    parser.add_argument("--binary", help="simforge CLI for spawned env serve (default $SIMFORGE_BIN, then PATH)")
    parser.add_argument("--num-envs", type=int, default=8)
    parser.add_argument("--total-timesteps", type=int, default=200_000)
    parser.add_argument("--rollout", type=int, default=128, help="steps per env per update")
    parser.add_argument("--epochs", type=int, default=10)
    parser.add_argument("--minibatch", type=int, default=256)
    parser.add_argument("--lr", type=float, default=3e-4)
    parser.add_argument("--gamma", type=float, default=0.99)
    parser.add_argument("--eval-every", type=int, default=5, help="updates between evaluations")
    parser.add_argument("--eval-seeds", default="1001,1002", help="held-out episode seeds per scenario")
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--threads", type=int, default=1, help="torch CPU threads")
    parser.add_argument("--vectorization", choices=("async", "sync"), default="async")
    parser.add_argument("--server-replicas", type=int, default=1, help="env serve processes per spawned scenario (one CPU core each)")
    parser.add_argument("--checkpoint", help="final policy file (default OUT/policy.pt, else ./ppo.pt)")
    parser.add_argument("--out", help="evidence directory: curve.csv, curve.svg, metadata.json")
    parser.add_argument("--json", action="store_true", help="print the summary document on stdout")
    args = parser.parse_args(argv)
    try:
        import torch
    except ImportError:
        raise SystemExit("the PPO example needs torch: pip install 'simforge-rl[ppo]'")

    torch.manual_seed(args.seed)
    torch.set_num_threads(args.threads)
    scenario_set = scenario_set_from(args)
    servers: ScenarioServers | None = None
    workdir = tempfile.TemporaryDirectory(prefix="sfrl-ppo-")
    runtime: RuntimeSessions | None = None
    try:
        if args.runtime is not None:
            runtime = RuntimeSessions(scenario_set, "--no-sensors", binary=args.runtime or None, map_dir=args.map_dir, workdir=workdir.name)
            transports: Any = runtime.transports()
        else:
            sessions = -(-args.num_envs // args.server_replicas) + 1
            servers = ScenarioServers(scenario_set, "--no-sensors", sessions=sessions, replicas=args.server_replicas, map_dir=args.map_dir, binary=args.binary)
            transports = servers.sockets
        return train(args, torch, scenario_set, transports, servers)
    finally:
        if runtime is not None:
            runtime.close()
        if servers is not None:
            servers.close()
        workdir.cleanup()


def train(args: Any, torch: Any, scenario_set: ScenarioSet, transports: Any, servers: ScenarioServers | None) -> int:
    started = time.time()
    out = Path(args.out) if args.out else None
    if out is not None:
        out.mkdir(parents=True, exist_ok=True)
    vector_sources = servers.socket_sets(args.num_envs) if servers is not None else transports
    envs = make_vector_env(scenario_set, vector_sources, args.num_envs, vectorization=args.vectorization, max_open=args.max_open)
    try:
        eval_env = ScenarioSetEnv(scenario_set, transports, max_open=args.max_open)
    except BaseException:
        envs.close()
        raise
    try:
        return learn(args, torch, scenario_set, servers, envs, eval_env, out, started)
    finally:
        envs.close()
        eval_env.close()


def learn(args: Any, torch: Any, scenario_set: ScenarioSet, servers: ScenarioServers | None, envs: Any, eval_env: ScenarioSetEnv, out: Path | None, started: float) -> int:
    eval_envs = {s.name: eval_env for s in scenario_set.scenarios}
    tasks = [(s.name, int(seed)) for s in scenario_set.scenarios[: args.eval_limit] for seed in args.eval_seeds.split(",")]
    hello = next(iter(eval_env.envs.values())).hello
    obs, _ = envs.reset(seed=args.seed)
    obs_size = features(obs).shape[-1]
    norm = RunningNorm(obs_size)
    norm.update(features(obs))
    policy = build_policy(torch, obs_size)
    optim = torch.optim.Adam(policy.parameters(), lr=args.lr, eps=1e-5)
    n = args.num_envs
    T = max(1, min(args.rollout, args.total_timesteps // n))
    updates = max(1, args.total_timesteps // (n * T))
    baseline = authored_baseline(eval_envs, tasks)
    baseline_mean = float(np.mean(list(baseline.values())))
    rows: list[dict[str, Any]] = []
    episode_returns: list[float] = []
    running = np.zeros(n)
    steps = 0

    def record(update: int) -> None:
        scores = evaluate(torch, policy, norm, eval_envs, tasks)
        recent = episode_returns[-4 * n:]
        rows.append({"update": update, "env_steps": steps, "eval_return": float(np.mean(list(scores.values()))),
                     "train_return": float(np.mean(recent)) if recent else None, "episodes": len(episode_returns),
                     **{f"eval_{k}": v for k, v in scores.items()}, "wall_s": round(time.time() - started, 1)})
        print(json.dumps(rows[-1]), file=sys.stderr, flush=True)

    record(0)
    for update in range(1, updates + 1):
        buf_obs, buf_act, buf_logp, buf_rew, buf_done, buf_val = [], [], [], [], [], []
        for _ in range(T):
            x = norm(features(obs))
            with torch.no_grad():
                xt = torch.as_tensor(x)
                dist = policy.dist(xt)
                action = dist.sample()
                logp = dist.log_prob(action).sum(-1)
                value = policy.critic(xt).squeeze(-1)
            obs, reward, terminated, truncated, info = envs.step(to_setpoint(action.numpy()))
            steps += n
            running += reward
            done = terminated | truncated
            bootstrap = np.zeros(n)
            if truncated.any() and "final_obs" in info:
                for i in np.flatnonzero(truncated & ~terminated):
                    final = {k: v for k, v in info["final_obs"][i].items()}
                    with torch.no_grad():
                        bootstrap[i] = float(policy.critic(torch.as_tensor(norm(features(final)[None])))[0, 0])
            for i in np.flatnonzero(done):
                episode_returns.append(float(running[i]))
                running[i] = 0.0
            norm.update(features(obs))
            buf_obs.append(x); buf_act.append(action.numpy()); buf_logp.append(logp.numpy()); buf_val.append(value.numpy())
            buf_rew.append(reward + args.gamma * bootstrap); buf_done.append(done)
        with torch.no_grad():
            last_value = policy.critic(torch.as_tensor(norm(features(obs)))).squeeze(-1).numpy()
        adv = np.zeros((T, n), dtype=np.float32)
        gae = np.zeros(n)
        for t in reversed(range(T)):
            next_value = last_value if t == T - 1 else buf_val[t + 1]
            nonterminal = 1.0 - buf_done[t]
            delta = buf_rew[t] + args.gamma * next_value * nonterminal - buf_val[t]
            gae = delta + args.gamma * 0.95 * nonterminal * gae
            adv[t] = gae
        ret = adv + np.asarray(buf_val)
        b_obs = torch.as_tensor(np.asarray(buf_obs).reshape(T * n, -1))
        b_act = torch.as_tensor(np.asarray(buf_act).reshape(T * n, -1))
        b_logp = torch.as_tensor(np.asarray(buf_logp).reshape(-1))
        b_adv = torch.as_tensor(adv.reshape(-1))
        b_ret = torch.as_tensor(ret.reshape(-1), dtype=torch.float32)
        rng = np.random.default_rng(args.seed * 100_003 + update)
        for _ in range(args.epochs):
            for idx in np.array_split(rng.permutation(T * n), max(1, T * n // args.minibatch)):
                i = torch.as_tensor(idx)
                dist = policy.dist(b_obs[i])
                ratio = (dist.log_prob(b_act[i]).sum(-1) - b_logp[i]).exp()
                a = (b_adv[i] - b_adv[i].mean()) / (b_adv[i].std() + 1e-8)
                pg = -torch.min(ratio * a, ratio.clamp(0.8, 1.2) * a).mean()
                vf = 0.5 * (policy.critic(b_obs[i]).squeeze(-1) - b_ret[i]).pow(2).mean()
                loss = pg + 0.5 * vf
                optim.zero_grad()
                loss.backward()
                torch.nn.utils.clip_grad_norm_(policy.parameters(), 0.5)
                optim.step()
        if update % args.eval_every == 0 or update == updates:
            record(update)
    checkpoint = Path(args.checkpoint) if args.checkpoint else (out / "policy.pt" if out else Path("ppo.pt"))
    torch.save({"policy": policy.state_dict(), "norm": {"mean": norm.mean, "var": norm.var, "count": norm.count}}, checkpoint)
    curve_digest = hashlib.sha256(json.dumps([{k: v for k, v in r.items() if k != "wall_s"} for r in rows], sort_keys=True).encode()).hexdigest()
    summary = {
        "schema": "simforge-rl.ppo-run/v1",
        "result": {"initialEvalReturn": rows[0]["eval_return"], "finalEvalReturn": rows[-1]["eval_return"],
                   "authoredControlReturn": baseline_mean, "authoredControlByScenario": baseline,
                   "finalEvalByScenario": {k[5:]: v for k, v in rows[-1].items() if k.startswith("eval_") and k != "eval_return"},
                   "envSteps": steps, "episodes": len(episode_returns), "curveSha256": curve_digest},
        "config": {k: v for k, v in vars(args).items() if k not in ("out", "checkpoint", "binary", "endpoint", "scenario_set", "export", "runtime", "map_dir", "json")},
        "transport": "runtime-stdio" if args.runtime is not None else "env-serve-socket",
        "map": None if scenario_set.map is None else {"name": scenario_set.map.name, "version": scenario_set.map.version, "xodrSha256": scenario_set.map.xodr_sha256},
        "scenarios": [{"name": s.name, "weight": s.weight, "seeds": list(s.seeds), "site": s.site,
                       "templateSha256": hashlib.sha256(Path(s.template).read_bytes()).hexdigest() if s.template else None,
                       "inputDigest": servers.servers[s.name][0].ready["input"]["digest"] if servers and s.name in servers.servers else None}
                      for s in scenario_set.scenarios],
        "evalTasks": [{"scenario": n_, "seed": s_} for n_, s_ in tasks],
        "runtime": {"protocol": hello["protocol"], "version": hello.get("version"), "engineVersion": hello.get("engineVersion"),
                    "engineHz": hello.get("engineHz"), "decisionHz": hello.get("decisionHz"), "mapXodrSha256": hello.get("map", {}).get("xodrSha256")},
        "environment": {"hostClass": "CPU", "cpuCount": os.cpu_count(), "python": platform.python_version(), "machine": platform.machine(),
                        "torch": torch.__version__, "numpy": np.__version__, "gymnasium": version("gymnasium"), "simforgeRl": version("simforge-rl")},
        "wallSeconds": round(time.time() - started, 1),
    }
    if out is not None:
        with (out / "curve.csv").open("w", newline="") as f:
            writer = csv.DictWriter(f, fieldnames=list(rows[0]))
            writer.writeheader()
            writer.writerows(rows)
        svg_curve(rows, baseline_mean, out / "curve.svg")
        (out / "metadata.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    if args.json:
        print(json.dumps(summary, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
