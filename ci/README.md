# Test Plan

## Tests

Rattan now contains two parts of tests:

- Standard Tests: run on each commit in all branches. (see .github/workflows/test.yaml)
- Verification Tests: run **only in the main branch** every day. (see .github/workflows/verify.yaml)

## Kernels

Rattan is currently tested on the latest four LTS kernels, that are 5.4 (the default one on Ubuntu 20.04), 5.15 (the default one on Ubuntu 22.04), 6.8 (the default one on Ubuntu 24.04) and 6.10 (the latest upstream release).

This directory contains artifacts setting up four test machines with equipped aforementioned four kernels as our development and CI environments.

| Machine Nickname | Kernel Version |     Distribution     |                                                                Cloud Image                                                                |
| :--------------: | :------------: | :------------------: | :---------------------------------------------------------------------------------------------------------------------------------------: |
|    focal-0505    |      5.4       | Ubuntu 20.04 (focal) | [focal/release-20240606](https://cloud-images.ubuntu.com/releases/focal/release-20230908/ubuntu-20.04-server-cloudimg-amd64-disk-kvm.img) |
|    jammy-0515    |      5.15      | Ubuntu 22.04 (jammy) | [jammy/release-20240612](https://cloud-images.ubuntu.com/releases/22.04/release-20230914/ubuntu-22.04-server-cloudimg-amd64-disk-kvm.img) |
|    noble-0608    |      6.8       | Ubuntu 24.04 (noble) |         [noble/20240521](https://cloud-images.ubuntu.com/releases/24.04/release-20240608/ubuntu-24.04-server-cloudimg-amd64.img)          |
|    noble-0612    |      6.12      | Ubuntu 24.04 (noble) |         [noble/20241119](https://cloud-images.ubuntu.com/releases/24.04/release-20241119/ubuntu-24.04-server-cloudimg-amd64.img)          |

## Prepare Virtual Machines

- Build machine images using [Packer by HashiCorp](https://www.packer.io/) with [libvirtd plugin](https://developer.hashicorp.com/packer/plugins/builders/libvirt) on a machine with `libvirtd` available.

First, install libvirtd using apt, and install Packer by following the [installation instruction](https://developer.hashicorp.com/packer/install).

```shell
# Installs libvirtd
sudo apt update
sudo apt install -y qemu-kvm libvirt-daemon-system libvirt-clients bridge-utils virtinst
sudo systemctl enable --now libvirtd
sudo usermod -aG libvirt,kvm $USER # re-login to make group settings take effect
```

Ansible is also required. You can install it by following [the official guide](https://docs.ansible.com/projects/ansible/latest/installation_guide/intro_installation.html#installing-and-upgrading-ansible-with-pipx), or you can install it using `uv`. The version is pinned to 2.18 so it still supports the Python 3.8 target on focal/20.04.

```shell
uv tool install --with ansible 'ansible-core>=2.18,<2.19'
```

Then, create the default pool to store the images created by packer:

```shell
virsh pool-define-as default dir --target /var/lib/libvirt/images
virsh pool-build default
virsh pool-start default
virsh pool-autostart default
virsh pool-list --all
```

Then, install required packer plugins. In `ci/images` directory:

```shell
# In ci/images
packer init .
```

Now, run `packer build` command for each distribution-kernel combination.

```shell
# In ci/images
# Note the dot at the last line
packer build \
  -var "github_access_key=<git_PAT>" \
  -var "release_name=<Distribution>" \
  -var "kernel_version=<Kernel>" \
  -var "key_import_user=<Public Key from GitHub User>" \
  -var "http_proxy=<Proxy Address>" \
  .
```

- `<git_PAT>` should be granted _Read and Write access to organization self hosted runners_.
- `<Distribution>` should be `jammy` (for Ubuntu 22.04), `focal` (for Ubutnu 20.04) or `noble` (Ubuntu 24.04).
- `<Kernel>` should be `5.4`, `5.15`, `6.8`, or `6.12`.
- `<GitHub User>` should be the user whose public keys will be imported to the VM.

If the guest must reach the internet through an HTTP proxy, add `-var "http_proxy=http://<host>:<port>"`.
This routes the mainline kernel downloads, and Docker (daemon image pulls, the runner
container, and containers spawned by CI jobs) through the proxy. Apt connections
are not proxied, if needed, uncomment the line at `image.pkr.hcl` by searching
for `apt`. Omit it for a direct connection.

If `packer build` command failed with error `Cloud not open '/var/lib/libvirt/images/<dist>-<kern_ver>`, refer to [this issue](https://github.com/dmacvicar/terraform-provider-libvirt/issues/1163), and follow user dylanf9797's solution on configuring apparmor to allow access to `/var/lib/libvirt/images`.

## Run Virtual Machines

Run VMs on a machine with `libvirtd` available. Must first create the image using packer.

```shell
# In ci/libvirt
virsh create ./libvirt/<Distribution>-<Kernel>.xml
```
