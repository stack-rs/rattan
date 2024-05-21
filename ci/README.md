# Test Plan

Rattan is currently tested on the latest four LTS kernels, that are 5.5 (the default one on Ubuntu 20.04), 5.10, 5.15 (the default one on Ubuntu 22.04) and 6.1.

This directory contains artifacts setting up four test machines with equipped aforementioned four kernels as our development and CI environments.

| Machine Nickname | Kernel Version | Distribution | Cloud Image |
| :---: | :---: | :---: | :---: |
| focal-0505 | 5.4 | Ubuntu 20.04 (focal) | [focal/release-20230209](https://cloud-images.ubuntu.com/releases/focal/release-20230908/ubuntu-20.04-server-cloudimg-amd64-disk-kvm.img) |
| jammy-0515 | 5.15 | Ubuntu 22.04 (jammy) | [jammy/release-20230302](https://cloud-images.ubuntu.com/releases/22.04/release-20230914/ubuntu-22.04-server-cloudimg-amd64-disk-kvm.img) |
| jammy-0601 | 6.1 | Ubuntu 22.04 (jammy) | [jammy/release-20230302](https://cloud-images.ubuntu.com/releases/22.04/release-20230914/ubuntu-22.04-server-cloudimg-amd64-disk-kvm.img) |
| noble-0606 | 6.6 | Ubuntu 24.04 (noble) | [noble/20240521](https://cloud-images.ubuntu.com/noble/20240521/noble-server-cloudimg-amd64.img) |

# Prepare Virtual Machines

* Build machine images using [Packer by HashiCorp](https://www.packer.io/) with [libvirtd plugin](https://developer.hashicorp.com/packer/plugins/builders/libvirt) on a machine with `libvirtd` available.

```shell
packer build -var "github_access_key=<git_PAT>" -var "release_name=<Distribution>" -var "kernel_version=<Kernel>" -var "key_import_user=<Public Key from GitHub User>" .
```

* `<git_PAT>` should be granted *Read and Write access to organization self hosted runners*.
* `<Distribution>` should be `jammy` (for Ubuntu 22.04), `focal` (for Ubutnu 20.04) or `mantic` (Ubuntu 24.04).
* `<Kernel>` should be `5.4`, `5.15`, `6.1` or `6.6`.
* `<GitHub User>` should be the user whose public keys will be imported to the VM.


Run VMs on a mchine with `libvirtd` availble.

```shell
virsh create ./libvirt/<Distribution>-<Kernel>.xml
```