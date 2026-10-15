# Parking and transit mechanisms

Scenario-template v2 examples: `parking.vehicle-pulls-out`, `parking.backing-out-vehicle`, `parking.delivery-double-park`, `parking.driveway-emergence`, `transit.bus-pullout`. `transit.bus-stop-emergence` is `examples/bus-stop-emergence.template.json` (not duplicated here).

Mechanism-bearing semantics are kept: the transit bus stays a `bus`; the double-parked cargo van is a static actor and occluder; the delivery worker is a `pedestrian`; backing out declares perpendicular bay orientation and reverse lamps; driveway visibility is constrained by a static hedge prop.
