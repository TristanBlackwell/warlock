# Gateway

Warlock functions as a control plane over a single host machine and possibly many guest VMs. Generally this is sufficient for basic setups and internal networks.

Warlock can operate in unison with [warlock-gateway](https://github.com/TristanBlackwell/warlock-gateway), an API proxy designed to coordinate requests between multiple Warlock worker nodes and provide centralized VM discovery. Defining the `GATEWAY_URL` variable described in the [configuration](../README.md#configuration) enables gateway integration.

## How It Works

When gateway integration is enabled, Warlock operates as a **worker node** that reports its state to the gateway:

1. **Worker Registration**: On startup, Warlock registers itself with the gateway using its `WORKER_ID` and `WORKER_IP`
2. **Heartbeat Monitoring**: Every 30 seconds, Warlock sends a heartbeat to prove it's alive and healthy
3. **VM Lifecycle Reporting**: 
   - When a VM is created, Warlock reports the VM ID and worker location to the gateway
   - When a VM is deleted, Warlock removes it from the gateway's registry
4. **Graceful Shutdown**: During shutdown, Warlock deregisters all VMs from the gateway

The gateway maintains a centralized registry of which worker hosts which VM, enabling:
- **SSH Connection Routing**: The gateway can route SSH connections to the correct worker based on VM ID
- **Multi-Worker Discovery**: Clients can query the gateway to find any VM across the cluster
- **Worker Health Monitoring**: The gateway tracks which workers are alive via heartbeats

## Graceful Degradation

Gateway integration is designed to be **optional and non-critical**:

- If `GATEWAY_URL` is not set, Warlock operates normally as a standalone control plane
- If the gateway is unreachable at startup, Warlock logs a warning and continues without gateway integration
- If gateway operations fail during runtime (VM registration, heartbeats), warnings are logged but VM operations succeed
- Core VM management (create, delete, SSH console) always works regardless of gateway availability

This ensures that **worker nodes remain operational even if the gateway is down**, maintaining high availability for local VM management and direct SSH access on port 2222.

## Configuration

See the main [Configuration](../README.md#configuration) documentation for gateway-related environment variables (`GATEWAY_URL`, `WORKER_ID`, `WORKER_IP`).