# API

Warlock exposes a HTTP API on port 3000. Endpoints accept and return JSON.

| Method | Path | Description |
|---|---|---|
| `GET` | `/internal/health` | Liveness probe -- returns `{"status": "ok"}` |
| `GET` | `/internal/ready` | Readiness probe -- returns status, capacity, VM count, copy strategy |
| `POST` | `/vm` | Create a VM |
| `GET` | `/vm` | List all VMs with state and resource allocation |
| `GET` | `/vm/{id}` | Get a specific VM's state |
| `DELETE` | `/vm/{id}` | Stop and delete a VM |


## Health

```bash
curl http://localhost:3000/internal/health
```

Minimal probe -- returns 200 if the process is alive.

## Readiness

```bash
curl http://localhost:3000/internal/ready
```

Returns host capacity, running VM count, allocated resources, and the detected rootfs copy strategy.

## Create VM

```bash
curl -X POST http://localhost:3000/vm \
  -H "Content-Type: application/json" \
  -d '{"vcpus": 2, "memory_mb": 256, "ssh_keys": ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA..."]}'
```

Creates a guest VM. Properties are optional but can be used to configure the machine. By default, 1 vCPU and 128 MB of memory will be allocated, which is also the minimum allowed resources. A successful response will return the ID of the machine and a `202` status code indicating the request was accepted and that the machine is booting.

`ssh_keys` can be used to produce SSH keys which the guest will accept connection requests from. If none are provided the machine will not be accessible via SSH.

If the host is at capacity, the request will be rejected with a `409` status code. There is no concept of scheduling or overallocation of resources such that the host must have capacity at the time of the request for creation to be successful.

## List VMs

```bash
curl http://localhost:3000/vm
```

Returns active guest VMs on the host and their allocated resources.

## List VM

```bash
curl http://localhost:3000/vm/{id}
```

Returns the state of an individual VM by it's ID.

## Delete VM

```bash
curl -X DELETE http://localhost:3000/vm/{id}
```

Deletes an individual VM by it's ID, gracefully shutting it down.