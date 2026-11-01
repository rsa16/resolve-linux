# Report Schema

`resolve-linux --report` writes a JSON report to stdout and exits 0.

The report is not specifically intended to be user-facing, but can function as such. The following is documentation describing the information the report contains. The report is generated to help diagnose compatibility issues that may arise with Resolve. `resolve-linux` uses the report to determine what modifications, if any, it needs to ensure compatibility.

| Key | Type | Meaning |
| --- | --- | --- |
| `schema_version` | integer | `1` for this schema. |
| `distro` | object | Detected distribution and whether `resolve-linux` supports it |
| `package_manager` | string or null | `apt-get`, `dnf`, `pacman`, or null (unsupported for now) |
| `resolve` | object | DaVinci Resolve installation state. |
| `dependencies` | array | Required dependency set |
| `gpu` | object | Support for the GPU |
| `patch` | object | Whether a patch is applicable or not. |

## `distro`

| Key | Type | Meaning |
| --- | --- | --- |
| `id` | string | The `ID` field from `os-release`. |
| `name` | string | `NAME` (falling back to `PRETTY_NAME`, then the ID). |
| `family` | string | `debian`, `fedora`, `arch`, or `unsupported`. |
| `supported` | boolean | Whether the distro is supported. |
| `manual_packages` | array of strings | Dependency names to install manually when unsupported. |
## `resolve`

| Key | Type | Meaning |
| --- | --- | --- |
| `installed` | boolean | Whether a Resolve installation root was found. |
| `version` | string or null | Version when known; null when not installed. |
| `tested` | boolean | Whether this version is one the tool has already been applied on. |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success (unsupported distro as well). |
| 1 | Detection or output failed. |
| 2 | Invalid arguments or execution as root. |
