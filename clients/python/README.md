# simforge-cloud

Synchronous, typed Python access to the SimForge Cloud `/api/v1` API. Apache-2.0. Dependencies: `httpx`, `pydantic`.

## Quickstart

```python
from simforge_cloud import SimforgeClient
client = SimforgeClient(base_url="https://simforge.ai")  # SIMFORGE_API_KEY supplies auth
scenarios = list(client.scenarios.list())
scenario = scenarios[0]
plan = client.runs.create(type="noop", input={"scenarioId": scenario.id}, dry_run=True)
print(plan.jobs)
run = client.runs.create(type="noop", input={"scenarioId": scenario.id}, dry_run=False)
run = client.runs.wait(run.id, timeout=120)
artifact = next(iter(client.artifacts.list(run.id)))
client.artifacts.download(artifact.id, "./" + artifact.name)
```

The API key may be supplied with `SIMFORGE_API_KEY` instead of `api_key`. Pass `workspace_id` when using a session or credential whose workspace is not already bound.

Resources: `health`, `me`, `workspaces`, `usage`, `api_keys` (create returns the secret once), `uploads`, `recordings`, `scenarios`, `maps`, `vehicles`, `datasets`, `runs` (create, dry run, events, cancel, wait), `artifacts` (sha256-verified download), `exports` (create or estimate with `dryRun`, file pages, retry, cancel), `drafts` (create, read, transact, save, freeze revision, validate) and `authoring` (catalogue, capabilities, spawn, discovery). Request bodies accept a mapping or the generated request model, sent with its wire aliases. Errors raise `SimforgeApiError` with the envelope's `code`, `reason`, `detail`, `path` and request id.

Interactive Gymnasium episodes run locally through `simforge-rl` (`clients/rl`); the cloud API has no interactive environment endpoint.

## Development

The generator emits resource modules under `simforge_cloud.models`; all public model names remain available from that namespace and from `simforge_cloud`. Input and output models use the catalogue-derived OpenAPI schema, and `--check` detects stale, missing or extra generated modules.

```sh
uv run --project clients/python --extra dev python -m pytest -q
uv run --project clients/python --extra dev python clients/python/scripts/generate_models.py --check
uv build clients/python
```
