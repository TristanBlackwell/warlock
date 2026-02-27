dev:
	WARLOCK_DEV=true cargo run

start:
	cargo run

test:
	cargo test

test-live:
	WARLOCK_LIVE=true cargo test --test vm_lifecycle -- --nocapture

droplet:
	./scripts/setup-droplet.sh

droplet-destroy:
	doctl compute droplet delete warlock-dev --force
