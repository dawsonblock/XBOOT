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
ROOTFS_IMAGE_SIZE_MIB ?= 256

.PHONY: build test guest-python guest-node image-python image-node template-python template-node verify clean

build:
	$(CARGO) build --release

test:
	$(CARGO) test

guest-python:
	./scripts/build_guest_rootfs.sh python $(PY_STAGING) $(if $(PY_ROOTFS_TEMPLATE),--rootfs-template $(PY_ROOTFS_TEMPLATE),) --write-manifest $(PY_STAGING_MANIFEST)

guest-node:
	./scripts/build_guest_rootfs.sh node $(NODE_STAGING) $(if $(NODE_ROOTFS_TEMPLATE),--rootfs-template $(NODE_ROOTFS_TEMPLATE),) --write-manifest $(NODE_STAGING_MANIFEST)

image-python: guest-python
	./scripts/build_rootfs_image.sh $(PY_STAGING) $(PY_ROOTFS) $(ROOTFS_IMAGE_SIZE_MIB)

image-node: guest-node
	./scripts/build_rootfs_image.sh $(NODE_STAGING) $(NODE_ROOTFS) $(ROOTFS_IMAGE_SIZE_MIB)

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
