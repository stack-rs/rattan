variable "github_access_key" {
  type = string
  sensitive = true
  default = ""
}

variable "base_images" {
  type = map(string)
  default = {
    "focal" = "https://cloud-images.ubuntu.com/releases/focal/release-20240606/ubuntu-20.04-server-cloudimg-amd64-disk-kvm.img"
    "jammy" = "https://cloud-images.ubuntu.com/releases/22.04/release-20240612/ubuntu-22.04-server-cloudimg-amd64-disk-kvm.img"
    "noble" = "https://cloud-images.ubuntu.com/releases/24.04/release-20240608/ubuntu-24.04-server-cloudimg-amd64.img"
  }
}

variable "base_images_checksum" {
  type = map(string)
  default = {
    "focal" = "1b9c3c313f87a65a15da3e8085d3c5fbb46d4052112f4363968f06f485a692aa"
    "jammy" = "9bc878ce5cc52128028db76719b68487b4269154f7b2882f38ebc6c64497850b"
    "noble" = "bc984eb1d1efbf2da8c7e9fa2487347a4cbc03247c487d890cdd32f231e1b3b0"
  }
}

variable "default_mac_address" {
  type = map(string)
  default = {
    "5.4" = "52:54:05:04:31:80"
    "5.15" = "52:54:05:15:31:80"
    "6.8" = "52:54:06:06:31:80"
    "6.10" = "52:54:06:10:31:80"
  }
}

variable "release_name" {
  type = string
}

variable "kernel_version" {
  type = string
} 

variable "key_import_user" {
  type = string
}

variable "install_runner" {
  type = bool
  default = true
}