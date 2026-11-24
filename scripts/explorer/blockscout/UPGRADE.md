# Stage an explorer upgrade

Use the upgrade overlay on a staging host with access to the selected chain's RPC. The overlay uses its own Compose project and database volumes and publishes only a loopback port. It requires Docker Compose 2.24.4 or newer for the port override.

## Candidate images

The public `latest` tags used by the base Compose file still point to backend 9.0.2 and frontend 2.3.5. Select and verify both candidate images before staging. A candidate to qualify is [Blockscout backend 12.1.0](https://github.com/blockscout/blockscout/releases/tag/v12.1.0) with Phoenix 1.6.18 and [frontend 2.13.0](https://github.com/blockscout/frontend/releases/tag/v2.13.0) with Next.js 16.4.6. The stock source tags pin earlier patch versions of those dependencies. The newer frontend requires a Blockscout API at version 12.0.0 or later.

The newer [backend license](https://github.com/blockscout/blockscout/blob/v12.1.0/LICENSE) and [frontend license](https://github.com/blockscout/frontend/blob/v2.13.0/LICENSE) govern use of those builds. Verify the terms for public use and distribution before promoting either image.

## Restore and start the candidate

1. In this directory, select `envs/zksys.env` for Genesis first. Create a separate private staging env file with fresh `POSTGRES_PASSWORD` and `SECRET_KEY_BASE` values. Use a URL-safe PostgreSQL password, such as hex, because Compose includes it in connection URLs. Set `L2_RPC_URL` in that file if the default Genesis indexer RPC is unavailable from the staging host. Set the following image and port fields in the same file.

   ```dotenv
   BLOCKSCOUT_UPGRADE_BACKEND_IMAGE=registry.example.org/explorer/backend
   BLOCKSCOUT_UPGRADE_BACKEND_SHA256=<64 hex characters from the backend digest>
   BLOCKSCOUT_UPGRADE_FRONTEND_IMAGE=registry.example.org/explorer/frontend
   BLOCKSCOUT_UPGRADE_FRONTEND_SHA256=<64 hex characters from the frontend digest>
   BLOCKSCOUT_UPGRADE_PORT=18082
   ```

2. Start PostgreSQL in the new project and restore a custom-format dump into its empty database. Confirm the two candidate refs in `config --images` before starting either image.

   ```bash
   stage=(docker compose --project-name blockscout-upgrade-57057
     --env-file envs/zksys.env --env-file /path/to/genesis-upgrade.env
     --profile user-ops -f docker-compose.yml -f docker-compose.upgrade.yml)

   "${stage[@]}" config --quiet
   "${stage[@]}" config --images
   "${stage[@]}" up -d --wait db redis-db
   "${stage[@]}" exec -T db pg_restore --exit-on-error --no-owner --no-privileges \
     -U blockscout -d blockscout < /path/to/genesis.dump
   "${stage[@]}" up -d backend
   "${stage[@]}" logs backend
   ```

3. The backend runs the database migrations on startup. Once it is healthy, start the remaining services. Check the backend version, indexed head against the chain RPC, token pagination used by the Portal, contract verification, and the Genesis user-operation pages. [Blockscout v12 API changes](https://docs.blockscout.com/blockscout-v12-0-0-api-updates-and-breaking-changes)

   ```bash
   "${stage[@]}" up -d
   curl -fsS http://127.0.0.1:18082/api/health
   curl -fsS http://127.0.0.1:18082/api/v2/config/backend-version
   curl -fsS http://127.0.0.1:18082/api/v2/blocks
   ```

4. Repeat for Gateway with `envs/gateway.env`, project `blockscout-upgrade-57001`, a separate staging env file and snapshot, a free loopback port, and without the Genesis user-ops profile. Use an RPC reachable from the staging host and verify that the Gateway proxy still restricts RPC access.

The existing remote deployment script uses the base Compose file. Promotion needs a separate rollout with a fresh rollback snapshot. If that rollout needs to be rolled back, restore the database snapshot along with the previous image pair.
