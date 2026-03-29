CARGO ?= cargo
ROOT := $(shell pwd)
PY_WORKDIR ?= $(ROOT)/work/python
NODE_WORKDIR ?= $(ROOT)/work/node
PY_STAGING ?= $(ROOT)/build/staging/python
NODE_STAGING ?= $(ROOT)/build/staging/node
PY_STAGING_MANIFEST ?= $(ROOT)/build/staging/python.manifest
NODE_STAGING_MANIFEST ?= $(ROOT)/build/staging/node.manifest
KERNEL ?= $(ROOT)/guest/vmlinux-fc
PY_ROOTFS ?= $(ROOT)/guest/rootfs-python.ext4
NODE_ROOTFS ?= $(ROOT)/guest/rootfs-node.ext4
PY_ROOTFS_TEMPLATE ?=
NODE_ROOTFS_TEMPLATE ?=
ROOTFS_IMAGE_SIZE_MIB ?=

.PHONY: build test guest-python guest-node image-python image-node template-python template-node verify clean \
    docker-build docker-build-runtime docker-run docker-smoke docker-compose-up docker-compose-down

# =============================================================================
# Docker Targets (Phase B)
# =============================================================================

DOCKER_IMAGE_NAME ?= xboot-runtime
DOCKER_IMAGE_TAG ?= latest
DOCKER_COMPOSE_FILE ?= deploy/docker/docker-compose.yml
DOCKER_ENV_FILE ?= deploy/docker/.env

docker-build: docker-build-runtime

docker-build-runtime:
	docker build \
		-f deploy/docker/Dockerfile.runtime \
		-t $(DOCKER_IMAGE_NAME):$(DOCKER_IMAGE_TAG) \
		.

docker-run: docker-build-runtime
	docker run \
		--device /dev/kvm \
		--privileged \
		--cgroupns=host \
		-p 8080:8080 \
		-v $(shell pwd)/target/release:/var/lib/zeroboot/current/bin:ro \
		-v $(shell pwd)/work/python:/var/lib/zeroboot/current/templates/python:ro \
		-v $(shell pwd)/work/node:/var/lib/zeroboot/current/templates/node:ro \
		-v $(shell pwd)/deploy/docker/secrets:/etc/zeroboot:ro \
		-v $(shell pwd)/deploy/docker/state:/var/lib/zeroboot \
		-e ZEROBOOT_AUTH_MODE=prod \
		-e ZEROBOOT_API_KEYS_FILE=/etc/zeroboot/api_keys.json \
		-e ZEROBOOT_API_KEY_PEPPER_FILE=/etc/zeroboot/pepper \
		-e ZEROBOOT_REQUEST_LOG_PATH=/var/lib/zeroboot/requests.jsonl \
		-e ZEROBOOT_LOG_CODE=false \
		-e ZEROBOOT_REQUIRE_TEMPLATE_HASHES=true \
		-e ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES=false \
		$(DOCKER_IMAGE_NAME):$(DOCKER_IMAGE_TAG)

docker-smoke:
	@echo "Running smoke test against Docker container..."
	./scripts/smoke_exec.sh test-key http://127.0.0.1:8080

docker-compose-up:
	@if [ ! -f $(DOCKER_ENV_FILE) ]; then \
		echo "Creating .env from .env.example..."; \
		cp deploy/docker/.env.example $(DOCKER_ENV_FILE); \
	fi
	@if [ ! -d deploy/docker/secrets ]; then \
		echo "Creating secrets directory..."; \
		mkdir -p deploy/docker/secrets; \
		python3 scripts/make_api_keys.py --count 1 --out deploy/docker/secrets/api_keys.json; \
		openssl rand -hex 32 > deploy/docker/secrets/pepper; \
		echo "Generated API keys and pepper secret"; \
	fi
	docker compose -f $(DOCKER_COMPOSE_FILE) --env-file $(DOCKER_ENV_FILE) up --build -d

docker-compose-down:
	docker compose -f $(DOCKER_COMPOSE_FILE) --env-file $(DOCKER_ENV_FILE) down

docker-compose-logs:
	docker compose -f $(DOCKER_COMPOSE_FILE) --env-file $(DOCKER_ENV_FILE) logs -f

docker-clean:
	docker rmi $(DOCKER_IMAGE_NAME):$(DOCKER_IMAGE_TAG) 2>/dev/null || true
	rm -rf deploy/docker/state

build:
	$(CARGO) build --release

test:
	$(CARGO) test

guest-python:
	./scripts/build_guest_rootfs.sh python $(PY_STAGING) $(if $(PY_ROOTFS_TEMPLATE),--rootfs-template $(PY_ROOTFS_TEMPLATE),) --write-manifest $(PY_STAGING_MANIFEST)

guest-node:
	./scripts/build_guest_rootfs.sh node $(NODE_STAGING) $(if $(NODE_ROOTFS_TEMPLATE),--rootfs-template $(NODE_ROOTFS_TEMPLATE),) --write-manifest $(NODE_STAGING_MANIFEST)

image-python: guest-python
	./scripts/build_rootfs_image.sh $(PY_STAGING) $(PY_ROOTFS) $(if $(ROOTFS_IMAGE_SIZE_MIB),$(ROOTFS_IMAGE_SIZE_MIB))

image-node: guest-node
	./scripts/build_rootfs_image.sh $(NODE_STAGING) $(NODE_ROOTFS) $(if $(ROOTFS_IMAGE_SIZE_MIB),$(ROOTFS_IMAGE_SIZE_MIB))

template-python: build
	mkdir -p $(PY_WORKDIR)
	./target/release/zeroboot template $(KERNEL) $(PY_ROOTFS) $(PY_WORKDIR) 20 /init 512

template-node: build
	mkdir -p $(NODE_WORKDIR)
	./target/release/zeroboot template $(KERNEL) $(NODE_ROOTFS) $(NODE_WORKDIR) 20 /init 512

verify:
	./verify.sh

clean:
	rm -rf build work target
