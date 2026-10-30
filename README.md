# hems-core

Read-only Home Assistant energy dashboard with local SQLite history.

## Step 7: advisory modelling

`GET /api/modelling/summary` reads Step 6 schema v1; it never calls Home Assistant,
changes device settings, schedules charging or writes model results. The dashboard
has a manual **Load modelling summary** view with demand ranges, battery energy,
SOC scenarios, source diagnostics and seven completed days of per-array comparisons.

The endpoint uses one bounded indexed read per metric over 28 days (maximum 40,321
rows per metric), serialized through the existing blocking SQLite worker. A
60-second single-flight cache limits repeated work; concurrent uncached requests
return 503. No additional periodic tasks or SQLite writes are introduced.

### Configure before relying on results

Copy the model settings from `.env.example` into the ignored service environment.

- Confirm nominal and usable battery capacities. Defaults 10.24 and 9.728 kWh are
  published starting assumptions, not learned measurements. Confirm whether your
  inverter SOC scale already excludes a reserve; the model assumes linear SOC
  across configured usable capacity and subtracts the configured reserve once.
- Confirm actual reserve (default 10%). No inverter settings are read or changed.
- Measure the essential CCTV/network load and set `HEMS_MODEL_ESSENTIAL_BASELOAD_W`
  if a minimum floor is needed. Default zero adds no extra floor; these loads remain
  included in whole-home history. The floor takes the maximum, never adds twice.
- Leave `HEMS_MODEL_WFH_DAYS` empty when unknown, or supply ISO weekdays such as
  `1,3`. This schedule applies to historical dates too: it is an assumption, not
  occupancy detection. Use `HEMS_MODEL_OCCUPANCY_DATES` JSON date labels (`home`,
  `away`, `wfh`) for variable occupancy; labels override the schedule on London
  dates. Supply historical labels as well as future ones to learn comparable groups.
- Confirm household-load and FIT SW / hybrid NW / hybrid W mappings using the
  existing collection settings. Keep tokens and meter identifiers in ignored `.env`.
- Retain enough history: default retention is 30 days. Shorter retention reduces
  model coverage. Demand training currently expects at least 30 accepted samples
  per UTC hour and three distinct London dates per local hour/occupancy group.
  Cadences slower than two minutes generally cannot satisfy this conservative gate.

### Methods and limits

Demand uses equally weighted daily hourly means with median and descriptive
10th/90th percentiles, grouped by local hour and occupancy. The repeated autumn
hour counts as one local date after averaging its two UTC-hour means. The spring
missing hour is not invented. Profiles identify overnight (00–06), daytime (07–16),
evening (17–23), weekends and occupancy labels. There is no ML, appliance inference,
occupancy inference or assumed away saving. Small groups are withheld rather than
pooled into a potentially misleading fallback.

The short horizon covers the current partial half hour followed by five half hours
(up to three hours), using UTC elapsed time with London offset labels. SOC is an
explicit **no-solar, no-charging, lossless, unlimited-discharge scenario**, not a
solar-aware operational prediction. Descriptive demand ranges produce scenario
SOC bounds; these are not calibrated confidence intervals. SOC stops at reserve
(or its initial value if already below reserve); grid supply would be needed beyond
that point. Missing demand breaks subsequent SOC propagation. SOC older than five
minutes is withheld. Battery rates are median/p90 nonzero reported power, not
hardware limits. Measured efficiency is null: the current schema has no verified
independent same-boundary energy channels. Capacity learning and efficiency
learning require suitable telemetry and validation before implementation.

Training rejects stale/missing/assumed-zero readings, wrong units, invalid SOC and
reports older than 300 seconds at collection (future tolerance 60 seconds). Same-
inverter activity alone does not refresh battery measurement timestamps. Accepted
sample counts and profile day counts are returned; no missing value becomes zero.

Solar comparisons use the latest revision **first seen before target-day London
midnight**. Provider issuance remains null where Step 6 has no verified issue time;
first-seen is an availability timestamp, not a fabricated provider issuance time.
No post-day revision can improve historical scores. Actuals integrate adjacent
power samples only across gaps of at most 120 seconds and require 95% coverage of
the full 23/24/25-hour London day. Missing night reports can therefore withhold a
whole-day comparison, deliberately. Partial integrated energy is not scaled up.
FIT actuals are AC output; hybrid actuals are DC input estimates. They are labelled
separately and are not used to infer battery charging or inverter efficiency.
Daily forecast totals cannot support a defensible intraday solar/SOC forecast.

### Verification

Run `cargo fmt --check`, `cargo check`, `cargo test`, then in `frontend/` run
`npm run check`, `npm test`, `npm run build`. Frontend distribution files are tracked
and must be rebuilt after source edits. Tests use synthetic local fixtures, never
live HA credentials or device writes. Service deployments should preserve their
existing local `.env` and SQLite files.
