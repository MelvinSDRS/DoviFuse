These reduced exports are generated from the vendored upstream `dovi_tool`
fixture binaries with:

```text
dovi_tool export --data all=out.json <fixture>.bin
jq -c '.[0] | {dovi_profile,el_type,header,rpu_data_mapping}' out.json
```

The source exporter is pinned at upstream commit `b25558062e4a56973482ec70133bd7b891320e48`.
The fixtures retain the complete header and mapping object needed by the
mapping policy tests while omitting unrelated VDR metadata.
