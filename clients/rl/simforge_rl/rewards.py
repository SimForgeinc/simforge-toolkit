"""Reward hooks: ``fn(native_reward, info) -> float`` over the engine's ``info["reward_terms"]``."""

from __future__ import annotations

from typing import Any, Callable, Mapping

RewardFn = Callable[[float, Mapping[str, Any]], float]


def native(reward: float, info: Mapping[str, Any]) -> float:
    """The runtime's own reward, unchanged."""
    return reward


class WeightedTerms:
    """``bias + sum(weights[k] * reward_terms[k])``; an unknown weighted term is an error."""

    def __init__(self, weights: Mapping[str, float], *, bias: float = 0.0) -> None:
        self.weights = dict(weights)
        self.bias = float(bias)

    def __call__(self, reward: float, info: Mapping[str, Any]) -> float:
        terms = info["reward_terms"]
        missing = set(self.weights) - set(terms)
        if missing:
            raise KeyError(f"reward terms {sorted(missing)} are not in this episode's {sorted(terms)}")
        return self.bias + sum(w * float(terms[k]) for k, w in self.weights.items())


class EventPenalty:
    """Wrap ``base`` and subtract ``penalty`` for each engine event whose ``kind`` is listed."""

    def __init__(self, kinds: Mapping[str, float], base: RewardFn = native) -> None:
        self.kinds = dict(kinds)
        self.base = base

    def __call__(self, reward: float, info: Mapping[str, Any]) -> float:
        events = info.get("events")
        if events is None:
            raise KeyError("EventPenalty needs info_channel=True (engine events)")
        hit = sum(self.kinds.get(str(e.get("kind")), 0.0) for e in events if isinstance(e, Mapping))
        return self.base(reward, info) - hit
