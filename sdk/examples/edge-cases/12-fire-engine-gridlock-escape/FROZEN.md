# Frozen records

`cell.json`, `input.json`, `scenario.trace.json.gz` and `validation.json` here
are frozen records of engine 0.6.0 on a Yale Street build that predates the
map registry (engine graph `fbebbdcc`). No current map version reproduces them:
their lanes are gone, and the template matches no site on any installed map.

On 2026-10-05 the template's `fire-engine` role was corrected (`vehicle.ambulance` with the ambulance's dims on a `car` role; now `vehicle.fire_engine` as class `truck` with its catalog dims (10.2 x 2.55 x 3.3 m)). The records
still show the old binding (`catalog:vehicle.ambulance`). They are kept as they
were, not regenerated. Re-authoring this example for a current map is on the
owner's backlog.
