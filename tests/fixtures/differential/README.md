# Differential tests against SQL Compare / SQL Data Compare

`tests/differential.rs` runs `quill schema` / `quill data` and the real `SQLCompare.exe` /
`SQLDataCompare.exe` on the same databases and checks that they reach the same outcome. Each
folder here is one scenario; `known.toml` lists the differences we know about.

## Running

The test is skipped unless all of these are set:

| Variable | Value |
|---|---|
| `QUILL_TEST_SERVER`, `QUILL_TEST_USER`, `QUILL_TEST_PASSWORD` | the test server, as for `compare_live.rs` |
| `QUILL_TEST_SQLCOMPARE` | path of `SQLCompare.exe` (version 15) |
| `QUILL_TEST_SQLDATACOMPARE` | path of `SQLDataCompare.exe` (version 15) |
| `QUILL_TEST_CONTAINER` | optional: Docker container of the server, for `backup` sources (default `quill-mssql-2022`) |
| `QUILL_DIFF_ONLY` | optional: comma-separated parts of scenario names to run |
| `QUILL_DIFF_JOBS` | optional: scenarios run at the same time (default 4) |
| `QUILL_DIFF_KEEP` | optional: keep the scenario databases for inspection (drop them yourself) |
| `QUILL_DIFF_REPORT` | optional: where to write the results (default `%TEMP%\quill-diff-<pid>\differential-results.md`) |

```powershell
$env:QUILL_TEST_SERVER = '127.0.0.1,14333'; $env:QUILL_TEST_USER = 'sa'; $env:QUILL_TEST_PASSWORD = '...'
$env:QUILL_TEST_SQLCOMPARE = 'C:\...\SQL Compare 15\SQLCompare.exe'
$env:QUILL_TEST_SQLDATACOMPARE = 'C:\...\SQL Data Compare 15\SQLDataCompare.exe'
cargo test --release --test differential -- --nocapture
```

The password reaches the Vendor tools as `/password1` / `/password2` arguments at run time only;
the logs in the work folder (`%TEMP%\quill-diff-<pid>\<scenario>\log.txt`, one per scenario, with
every command line, exit code and output, and the scripts each tool wrote) have it masked.
Databases are named `QD_diff_<pid>_<scenario>_{1,2,q,r}` and dropped afterwards; logins a
scenario creates must be named `QD_diff_..._{pid}` so they are dropped too.

## A scenario

A folder with:

- `scenario.toml`: `kind = "schema"` or `"data"`, `about`, optional `finding` (the audit item it
  covers), `source` (`live` default, `scripts`, `snapshot`, `backup`), `args` (extra switches given
  to both tools), `judge_args` (switches for the judging runs; default `args` without
  `/include:Identical`), `project` (a `.scp` / `.sdc` template in the folder) and `oracles` (a
  subset to run).
- `db1.sql`, `db2.sql`: GO-separated scripts that build database 1 and database 2 (rows included).
- optional `check.sql`: a query run on both deployed copies; the rows must be equal.
- optional `server.sql`: run in master first.

Placeholders in scripts, switches and project templates: `{db}` (the database a script builds),
`{db1}`, `{db2}`, `{server}`, `{user}`, `{source1}` (path of the scripts folder / backup),
`{dir}` (work folder), `{fixture}` (scenario folder), `{pid}`.

Sources are made from database 1 with the Vendor tools: `scripts` with `SQLCompare.exe
/makescripts` (data scenarios and `/include:StaticData` then fill its `Data` folder with
`SQLDataCompare.exe /scripts2 /synchronize`), `snapshot` with `/makesnapshot`, `backup` with
`BACKUP DATABASE` in the container and `docker cp`.

## Oracles

Database 2 is built four times: `_2` for the comparison, `_q` for quill's deployment, `_r` for
Vendor's.

| Oracle | Passes when |
|---|---|
| `converge-quill` | quill deploys to `_q` (`/synchronize`) and Vendor then finds database 1 and `_q` identical (`/assertidentical`; data: exit 63) |
| `converge-vendor` | Vendor deploys to `_r` and quill finds database 1 and `_r` identical |
| `outcome` | both deployments end alike and Vendor finds `_q` and `_r` identical (only `/options` from `args`, no filters) |
| `check` | `check.sql` returns the same rows on `_q` and `_r` |
| `prints` | data: the two synchronization scripts print the same progress messages in the same order (runs with `outcome`) |
| `diffset` | schema: the same (object type, name, status) set in the XML reports; data: the same per-table counts of different / only in 1 / only in 2 / identical rows (a run with `/include:Identical`) |
| `names` | schema: the report `fqn` values are the same (catches quoting differences that `diffset` ignores) |
| `warnings` | the same set of warnings, by severity and text with identifiers masked |
| `exit` | the scenario's command line exits with the same code |

When both deployments fail with the same exit code, the two `converge` oracles pass.

Normalised as documented deviations: the product name in warnings (QuillSQL vs SQL Compare /
SQL Data Compare), script headers (scripts are not compared), the object-name prefix quill puts in
front of a warning (`[dbo].[T]: ...`), brackets in names for `diffset`, console width
(`/outputwidth:250` is passed to both). Both tools get the same connection switches: no
`/trustServerCertificate`, since SQL Compare has none and does not validate the certificate of an
unencrypted connection.

## Known differences

`known.toml` holds `[[known]]` entries with `scenario`, `oracle`, `reason` and `finding` (an
audit2 item such as `schema-model H1`, or `new`). A listed (scenario, oracle) that fails is an
expected failure; one that passes fails the test, so remove the entry once the engine is fixed.
`scenario = "*"` matches every scenario and must fail in at least one of them in a full run. A
failure that is not listed fails the test.

Not covered: Windows and Entra users (schema-model H2, schema-deploy H1) need a domain or Entra
ID, which the Linux test container does not have (`CREATE USER [DEV\svc]` fails there).
Tool runs that fail for machine reasons (no free sockets, Vendor's licence check crashing under
load) are retried.
