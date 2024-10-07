packer {
  required_plugins {
    sshkey = {
      version = ">= 1.0.1"
      source  = "github.com/ivoronin/sshkey"
    }
    libvirt = {
      version = "0.4.5"
      source  = "github.com/thomasklein94/libvirt"
    }
    ansible = {
      version = "~> 1"
      source = "github.com/hashicorp/ansible"
    }
  }
}

data "sshkey" "install" {
}

source "libvirt" "image" {
  libvirt_uri = "qemu:///system"

  vcpu   = 4
  memory = 8192

  network_interface {
    type  = "managed"
    alias = "communicator"
    mac = "${lookup(var.default_mac_address, var.kernel_version, "")}"
  }

  # https://developer.hashicorp.com/packer/plugins/builders/libvirt#communicators-and-network-interfaces
  communicator {
    communicator         = "ssh"
    ssh_username         = "rattan"
    ssh_private_key_file = data.sshkey.install.private_key_path
  }
  network_address_source = "lease"

  volume {
    alias = "artifact"
    target_dev = "sda"
    bus        = "sata"

    source {
      type = "external"
      # With newer releases, the URL and the checksum can change.
      urls     = [ "${lookup(var.base_images, var.release_name, "")}" ]
      checksum = "${lookup(var.base_images_checksum, var.release_name, "")}"
    }

    name       = "${var.release_name}-${var.kernel_version}"
    pool       = "default"
    capacity   = "32G"
    size       = "32G"
    format     = "qcow2"
  }

  volume {
    target_dev = "sdb"
    bus        = "sata"
    source {
      type = "cloud-init"
      user_data = format("#cloud-config\n%s", jsonencode({
        resize_rootfs = true
        growpart = {
          mode                     = "auto"
          devices                  = ["/"]
          ignore_growroot_disabled = false
        }

        users = [
          {
            name          = "rattan"
            sudo          = "ALL=(ALL) NOPASSWD:ALL"
            shell         = "/bin/bash"
            lock_passwd  = false
            hashed_passwd = "$6$rounds=4096$InVTnQ3fjMCSbc$ryRQrcU7ym0mvl.d7YxmR4HINu8/9u3XfG0KS4Ie59Pi8P5Xc9QoMRXOSVnEfpC4vJQn6Xa.2MHpBY6TeFZMH."
            ssh_import_id = [
              "gh:${var.key_import_user}"
            ]
            ssh_authorized_keys = [
              data.sshkey.install.public_key,
            ]
          }
        ]

        write_files = [
          {
            path = "/etc/modules-load.d/bbr.conf"
            content = "tcp_bbr"
          }
        ]

        apt = {
          sources = {
            mainline = {
              source = "ppa:cappelikan/ppa"
            }
          }
        }

        packages = [
          "policykit-1",
          "mainline",
          "ca-certificates"
        ]

        package_update             = true
        package_upgrade            = true
        package_reboot_if_required = true

        hostname = "${var.release_name}-${var.kernel_version}"
      }))
    }
    pool       = "default"
  }
  shutdown_mode = "acpi"
}

build {
  sources = ["source.libvirt.image"]
  provisioner "shell" {
    inline = [
      "echo The domain has started and became accessible",
      "echo The domain has the following addresses",
      "ip -br a",
      "echo if you want to connect via SSH use the following key: ${data.sshkey.install.private_key_path}",
    ]
  }
  provisioner "shell" {
    inline = [
      "/usr/bin/cloud-init status --wait"
    ]
    expect_disconnect = true
  }
  provisioner "breakpoint" {
    note = "You can examine the created domain with virt-manager, virsh or via SSH"
  }
  provisioner "shell" {
    inline = [
      "set -xo pipefail",
      "sudo mainline list | grep -E \"^[0-9]+\\.[0-9]+\\.[0-9]+\" | grep -E \"^${var.kernel_version}\" | head -n 1 | tr -d ' ' | sed -e 's/Installed//' | xargs -I {} sudo mainline install {}"
    ]
    inline_shebang = "/bin/bash -e"
    skip_clean = true
    valid_exit_codes = [0, 123, 141] # it's ok if the kernel is already what we want
  }
  provisioner "shell" {
    inline = [
      "([[ -f /usr/lib/python3.12/EXTERNALLY-MANAGED ]] && sudo rm -f /usr/lib/python3.12/EXTERNALLY-MANAGED) || true",
      "([[ -f /usr/lib/python3.11/EXTERNALLY-MANAGED ]] && sudo rm -f /usr/lib/python3.11/EXTERNALLY-MANAGED) || true",
    ]
    inline_shebang = "/bin/bash"
  }
  provisioner "breakpoint" {
    note = "You can examine the created domain with virt-manager, virsh or via SSH"
  }
  provisioner "ansible" {
    playbook_file = "./ansible/configure.yml"
    galaxy_file = "./ansible/requirements.yml"
    extra_arguments = [ "--extra-vars", "github_access_key=${var.github_access_key} kernel_version=${var.kernel_version} runner_name=${var.release_name}-${var.kernel_version} tag=ubuntu-${var.release_name} install_runner=${var.install_runner}" ]
  }
}