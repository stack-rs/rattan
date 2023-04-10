variable "github_access_key" {
  type = string
  sensitive = true
}

variable "base_images" {
  type = map(string)
  default = {
    "focal" = "https://cloud-images.ubuntu.com/releases/focal/release-20230209/ubuntu-20.04-server-cloudimg-amd64-disk-kvm.img"
    "jammy" = "https://cloud-images.ubuntu.com/releases/22.04/release-20230302/ubuntu-22.04-server-cloudimg-amd64-disk-kvm.img"
  }
}

variable "base_images_checksum" {
  type = map(string)
  default = {
    "focal" = "3f2dcab9c361c9832f3a61696b7748c36efc0f4b6d548e56a65921e4f1d3d6c7"
    "jammy" = "3b11d66d8211a8c48ed9a727b9a74180ac11cd8118d4f7f25fc7d1e4a148eddc"
  }
}

variable "default_mac_address" {
  type = map(string)
  default = {
    "5.4" = "52:54:05:04:31:80"
    "5.15" = "52:54:05:15:31:80"
    "6.1" = "52:54:06:01:31:80"
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