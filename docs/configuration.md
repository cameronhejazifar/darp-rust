# Configuration Guide

darp stores its configuration in `~/.darp/config.json`. You can edit this file directly or use `darp config` commands.

## Settings Resolution

When you run `darp serve` or `darp shell`, settings are resolved from most specific to least specific:

```
Service > Group > Domain > Environment
```

The first level that defines a setting wins. For example, if a service defines `serve_command`, the group/domain/environment values are ignored.

**For scalar settings** (serve_command, shell_command, image_repository, platform, default_container_image): the most specific value wins.

**For collection settings** (volumes, host_portmappings, variables): the most specific level that defines the collection wins entirely. Collections are not merged across levels.

## Environment Resolution

The environment is determined by:

1. The `-e` flag on the command line
2. The service's `default_environment`
3. The group's `default_environment`
4. The domain's `default_environment`

## Config Structure

```json
{
  "pre_config": [
    {
      "location": "{home}/team-repo/config.json",
      "repo_location": "{home}/team-repo"
    }
  ],
  "engine": "docker",
  "podman_machine": null,
  "urls_in_hosts": true,
  "domains": {
    "my-projects": {
      "location": "{home}/projects",
      "default_environment": "go",
      "groups": {
        ".": {},
        "laravel": {
          "default_environment": "lara:13",
          "services": {
            "admin": {
              "serve_command": "php artisan serve --host 0.0.0.0",
              "host_portmappings": { "8082": "8082" },
              "urls": ["local.admin.example.org"]
            }
          }
        }
      },
      "volumes": [...],
      "variables": {...},
      "host_portmappings": {...},
      "serve_command": "...",
      "shell_command": "...",
      "image_repository": "...",
      "platform": "...",
      "default_container_image": "..."
    }
  },
  "environments": {
    "go": {
      "serve_command": "air",
      "shell_command": "bash",
      "image_repository": "my-registry/go",
      "default_container_image": "1.25",
      "platform": "linux/amd64",
      "volumes": [...],
      "variables": {...},
      "host_portmappings": {...}
    }
  }
}
```

## In-container `/etc/hosts`

darp bind-mounts a managed hosts file over `/etc/hosts` inside every `darp shell` / `darp serve` container. That file includes standard loopback entries, a line for the container engine's host-gateway (`host.docker.internal` or `host.containers.internal` resolved to the platform-correct IP), and one `0.0.0.0 <service>.<domain>.test` line per service for intra-service reachability. The gateway IP is probed once by `darp install` and cached at `~/.darp/container_host_ip`; `darp deploy` re-probes automatically if the cache is missing or was written for a different engine.

## Tokens

These tokens are expanded at runtime:

| Token | Expands to |
|---|---|
| `{home}` | Your home directory (e.g. `/Users/you`) |
| `{pwd}` | The current project directory (volumes only) |

## Available Settings

These settings can be configured at the **environment**, **domain**, **group**, or **service** level:

| Setting | Description |
|---|---|
| `serve_command` | Command run by `darp serve` |
| `shell_command` | Shell used by `darp shell` (default: `sh`) |
| `image_repository` | Docker registry prefix (image becomes `repo:tag`) |
| `default_container_image` | Image used when none is passed on the CLI |
| `platform` | Container platform (e.g. `linux/amd64`) |
| `host_portmappings` | Map of `host_port: container_port` to expose |
| `variables` | Map of `name: value` environment variables |
| `volumes` | List of `{ container, host }` mount paths |

Additionally:

| Setting | Where | Description |
|---|---|---|
| `default_environment` | Domain, Group, Service | Fallback environment when `-e` isn't passed |
| `location` | Domain | Filesystem path to the domain folder |
| `urls` | Service | Extra hostnames that reach this service alongside `{service}.{domain}.test` |

## URL Aliases

A service normally answers on one hostname, `{service}.{domain}.test`, derived
from its folder name. `urls` adds more:

```json
"portal-website": {
  "urls": ["local.comagine.org", "local.zoo.org"]
}
```

`darp deploy` then writes a `/etc/hosts` entry and an nginx `server` block for
each alias, all proxying to the same upstream port as the canonical URL. `darp
urls` lists aliases indented under their service.

Aliases do not have to end in `.test` — darp writes a hosts entry for each one,
so any name resolves to the loopback the reverse proxy listens on. (Only `.test`
is wildcard-resolved by dnsmasq; everything else needs the hosts entry, which is
exactly what this provides.)

This exists for apps that behave differently per hostname — a multi-tenant
front-end that picks its tenant from `window.location.hostname`, for instance.
Because every alias reaches the same container on the same port, the app sees
the original hostname in the `Host` header and can branch on it.

`urls` is service-level only and applies at deploy time. It does not cascade
through group/domain/environment and has no `*urls` override form.

### Hostname rules

An alias value is a **hostname, not a URL**. `darp deploy` validates every alias
before it writes anything, and rejects the whole deploy — leaving the previous
deployment untouched — if any value is malformed. Every invalid alias across
every service is reported in one error.

Not allowed:

- Schemes (`http://`, `https://`), ports, paths, query strings, fragments, and
  user information.
- Whitespace, control characters, and characters that would alter nginx or
  hosts-file syntax (`;`, `{`, `}`, quotes, backslash).
- Wildcards such as `*.zoo.org`.
- IPv4 and IPv6 address literals — `urls` names alternate hostnames, not
  alternate listener addresses.
- Labels that are empty, longer than 63 characters, or that begin or end with a
  hyphen; hostnames longer than 253 characters.
- Raw non-ASCII names. Internationalized hostnames must be supplied in
  ASCII/Punycode form (`xn--…`).

Normalization applied to accepted values:

- Surrounding whitespace is trimmed.
- One optional trailing DNS dot is accepted and removed (`local.zoo.org.` →
  `local.zoo.org`).
- ASCII letters are lowercased, since DNS is case-insensitive.

The normalized value is what lands in the nginx `server_name`, the hosts files,
`portmap.json`, and `darp urls` output.

Valid:

```json
"urls": ["local.zoo.org", "tenant-a.portal.test"]
```

Invalid:

```json
"urls": [
  "https://local.zoo.org",
  "local.zoo.org:8080",
  "local.zoo.org/path"
]
```

## Viewing Resolved Config

To see what settings would apply at your current directory:

```sh
darp config show              # uses default environment
darp config show -e staging   # override environment
```

This outputs the fully resolved JSON after applying the Service > Group > Domain > Environment chain.
