# Junction and VRU mechanisms

Templates the matcher, materializer and engine preserve with explicit primitives: `intersection.right-turn-crosswalk`, `intersection.left-turn-crosswalk`, `intersection-blocked-box-reveal`, `vru.adult-midblock-crossing`, `vru.cyclist-crossing-path`.

- Turn/crosswalk templates need a map-derived turn and a receiving-side crossing; turn-crossing actors use the mapped crossing route end to end.
- The blocked-box template needs a geometric crossing gate and a separate adjacent lane; its queue van is both a static collision actor and the declared line-of-sight occluder for the cross-traffic car.
- The cyclist uses the `bicycle` kind end to end (never a pedestrian or generic vehicle substitute).
- Freeform VRU paths contain the exact lane-centre pose of their arrival invariant, then continue to the far road edge. No participant despawns at the conflict, so every rendition keeps its aftermath.

Not represented (do not attach these labels to weaker look-alikes): stop violation (no stop-line dwell-and-release controller or metric); late red-light entry (no general authoritative movement phase and stop-line timing); opposing-turn encroachment (no lane-boundary penetration or escape-width invariant); cyclist right hook (no binding proving an independent parallel path while ego turns); crossing-guard release (gestures do not control a pedestrian group).
