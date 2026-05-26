# UF2 tooling (vendored)

`uf2conv.py` and `uf2families.json` are vendored unmodified from the upstream
[microsoft/uf2](https://github.com/microsoft/uf2) repository.

- **Source**: <https://github.com/microsoft/uf2/tree/master/utils>
- **License**: MIT (see [`LICENSE`](LICENSE))
- **Copyright**: Microsoft Corporation

`uf2conv.py` reads the family table from `uf2families.json` in the same
directory as the script, so keep them together. Both files are used by the
`flash_uf2.sh` scripts under `firmware/sensor/` and `firmware/receiver/`.

## Updating

To pick up upstream fixes, replace both files from the latest release:

```bash
curl -L https://raw.githubusercontent.com/microsoft/uf2/master/utils/uf2conv.py -o uf2conv.py
curl -L https://raw.githubusercontent.com/microsoft/uf2/master/utils/uf2families.json -o uf2families.json
```
