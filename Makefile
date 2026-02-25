dev:
	WARLOCK_DEV=true cargo run

start:
	cargo run

test:
	cargo test

droplet:
	./scripts/setup-droplet.sh

droplet-destroy:
	doctl compute droplet delete warlock-dev --force
