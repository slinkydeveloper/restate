# Release Notes: Connect a local service to Restate Cloud from the CLI

## New Feature

### What Changed

Two commands take a service from your laptop into a Restate Cloud environment, with no change to its code:

```shell
restate cloud login            # once per machine; now also selects the environment
restate cloud link             # once per project: writes ./.env
npm run dev                    # serve() reads .env and connects to the environment
restate deployments register tunnel://greeter-my-laptop   # once; the SDK prints this line
```

- `restate cloud login` ends by selecting the Cloud environment the CLI talks to: silently when you have one account with one environment, with a prompt otherwise. Pass `--no-env` to only log in.
- `restate cloud link` mints an API key and writes the connection settings of the current Cloud environment (or `--env ACCOUNT/ENVIRONMENT`) to `.env` in the current directory: `RESTATE_ENVIRONMENT_ID`, `RESTATE_AUTH_TOKEN`, `RESTATE_CLOUD_REGION`, `RESTATE_SIGNING_PUBLIC_KEY`, `RESTATE_INGRESS_URL` and `RESTATE_TUNNEL_NAME` (the deployment name, `<directory>-<hostname>` unless you pass `--name`). Other lines of an existing `.env` are kept. It warns when the file isn't ignored by git, refuses to mint a second key for the same environment unless you pass `--force`, and can print the file instead with `--print` (for CI secrets) or write elsewhere with `--output`. The key has full access for now; it will get the narrower Connect role once Restate Cloud offers it.
- `restate deployments register tunnel://<deployment name>` registers a service that connected through `serve()`. The CLI builds the deployment URL from the current Cloud environment and your login; nothing is read from the project's `.env`.
- `restate env` is the new group for the environments the CLI talks to: `restate env list`, `restate env use <name>`, `restate env add` (a Cloud environment by default, a self-hosted server with `--manual`: in `$EDITOR`, or with `--admin-url`, `--ingress-url` and `--bearer-token` for scripts) and `restate env remove <name>`.
- `restate cloud tunnel` replaces `restate cloud env tunnel` for the port-forwarding tunnel (services that can't connect by themselves). `--tunnel-name` is now `--name` (the old spelling still works), and the default name is `<directory>-<hostname>` instead of a random word, so a restart reuses the same registered deployment.

All of them support `--json`, and the ones that change something support `--yes`.

## Deprecations

These commands keep working but are hidden from `--help` and print a deprecation line. They will be removed in a later release:

| Deprecated | Use instead |
|---|---|
| `restate config list-environments` | `restate env list` |
| `restate config use-environment` | `restate env use` |
| `restate cloud env configure` | `restate env add` |
| `restate cloud env tunnel` | `restate cloud tunnel` |

## Behavioral Change

### What Changed

The CLI no longer loads a `.env` file on its own. Until now it looked for one in the current directory and its parents and took `RESTATE_ADMIN_URL`, `RESTATE_INGRESS_URL`, `RESTATE_AUTH_TOKEN` and the other `RESTATE_*` variables from it.

### Why This Matters

The project `.env` now belongs to your service: `restate cloud link` writes an API key into it as `RESTATE_AUTH_TOKEN`, the same variable the CLI reads as its bearer token. Had the CLI kept loading the file, running `restate deployments register` from the project directory would have used that key instead of your login and failed.

### Impact on Users

- Nothing changes if you configure the CLI with real environment variables (as CI setups do), with the config file, or with `restate cloud login`.
- If you kept a `.env` for the CLI, its variables are no longer picked up. When the CLI finds a `./.env` with `RESTATE_*` variables it didn't load, it prints a one-line hint on stderr; it stays silent for files written by `restate cloud link` or the Cloud UI.

### Migration Guidance

Pick one:

```shell
# Add the server as an environment of the CLI, once (switch with `restate env use`)
restate env add --manual staging --admin-url https://restate.example.com:9070 \
    --ingress-url https://restate.example.com:8080 --bearer-token "$TOKEN" --yes

# Or load the file explicitly (a shell alias restores the old behavior)
restate --env-file .env whoami
```
