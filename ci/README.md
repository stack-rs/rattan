# Test Plan

## Tests

Rattan now contains two parts of tests:

* Standard Tests: run on each commit in all branches. (see .github/workflows/test.yaml) 
* Verification Tests: run **only in the main branch** every day. (see .github/workflows/verify.yaml)

## Kernels
Rattan is currently tested on the latest four LTS kernels, that are 5.4 (the default one on Ubuntu 20.04), 5.15 (the default one on Ubuntu 22.04), 6.8 (the default one on Ubuntu 24.04) and 6.9 (the latest upstream release).

This directory contains artifacts setting up four test machines with equipped aforementioned four kernels as our development and CI environments.

| Machine Nickname | Kernel Version | Distribution | Cloud Image |
| :---: | :---: | :---: | :---: |
| focal-0505 | 5.4 | Ubuntu 20.04 (focal) | [focal/release-20240606](https://cloud-images.ubuntu.com/releases/focal/release-20230908/ubuntu-20.04-server-cloudimg-amd64-disk-kvm.img) |
| jammy-0515 | 5.15 | Ubuntu 22.04 (jammy) | [jammy/release-20240612](https://cloud-images.ubuntu.com/releases/22.04/release-20230914/ubuntu-22.04-server-cloudimg-amd64-disk-kvm.img) |
| noble-0608 | 6.8 | Ubuntu 24.04 (noble) | [noble/20240521](https://cloud-images.ubuntu.com/releases/24.04/release-20240608/ubuntu-24.04-server-cloudimg-amd64.img) |
| noble-0609 | 6.9 | Ubuntu 24.04 (noble) | [noble/20240521](https://cloud-images.ubuntu.com/releases/24.04/release-20240608/ubuntu-24.04-server-cloudimg-amd64.img) |

# Prepare Virtual Machines

* Build machine images using [Packer by HashiCorp](https://www.packer.io/) with [libvirtd plugin](https://developer.hashicorp.com/packer/plugins/builders/libvirt) on a machine with `libvirtd` available.

```shell
packer build -var "github_access_key=<git_PAT>" -var "release_name=<Distribution>" -var "kernel_version=<Kernel>" -var "key_import_user=<Public Key from GitHub User>" .
```

* `<git_PAT>` should be granted *Read and Write access to organization self hosted runners*.
* `<Distribution>` should be `jammy` (for Ubuntu 22.04), `focal` (for Ubutnu 20.04) or `noble` (Ubuntu 24.04).
* `<Kernel>` should be `5.4`, `5.15` or `6.8`.
* `<GitHub User>` should be the user whose public keys will be imported to the VM.


Run VMs on a mchine with `libvirtd` availble.

```shell
virsh create ./libvirt/<Distribution>-<Kernel>.xml
```