# Corridor mechanism templates

v2 templates for longitudinal and same-direction lane conflicts, using only primitives the materializer and engine implement: `lead-hard-brake`, `queue-tail`, `cutout-reveals-stopped`, `cut-in-brake`, `sideswipe`, `merge-gap-collapse` (`*.template.json`).

Not represented (would overstate coverage):

- **Slow vulnerable lead:** no lateral passing-clearance metric in the invariant and trace vocabularies.
- **Lane-drop late merge:** the engine input carries no lane-end/taper boundary, no reaches-lane-end condition, no remaining-pavement invariant.
- **Oncoming overtake:** the lane-change controller resolves only same-direction lanes; it cannot cross the centreline and resolve a return gap.
