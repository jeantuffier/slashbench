# Vendored assets

- `chart.umd.min.js` — [Chart.js](https://www.chartjs.org) v4.5.1 (MIT, see
  `chart.js.LICENSE.md`).
- `chartjs-plugin-datalabels.min.js` — the official Chart.js datalabels plugin
  v2.2.0 (MIT, see `chartjs-plugin-datalabels.LICENSE.md`), used for always-
  visible value labels on bars per the report's mark spec.

Both fetched from jsDelivr/GitHub and vendored here rather than loaded from a
CDN at report-view time. `report.rs` embeds them at compile time via
`include_str!` so the generated report is a single self-contained HTML file —
no external requests, works fully offline, and won't break if a CDN link rots.
All chart rendering is delegated to these libraries — the Rust code only
builds their data/config objects, it does not draw charts itself.

To update either: fetch a newer pinned version and replace the file, e.g.

```
curl -sL https://cdn.jsdelivr.net/npm/chart.js@<version>/dist/chart.umd.min.js \
  -o chart.umd.min.js
curl -sL https://cdn.jsdelivr.net/npm/chartjs-plugin-datalabels@<version>/dist/chartjs-plugin-datalabels.min.js \
  -o chartjs-plugin-datalabels.min.js
```
