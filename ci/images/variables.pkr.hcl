variable "github_access_key" {
  type = string
  sensitive = true
}

variable "base_images" {
  type = map(string)
  default = {
    "focal" = "https://cloud-images.ubuntu.com/releases/focal/release-20230908/ubuntu-20.04-server-cloudimg-amd64-disk-kvm.img"
    "jammy" = "https://cloud-images.ubuntu.com/releases/22.04/release-20230914/ubuntu-22.04-server-cloudimg-amd64-disk-kvm.img"
    "mantic" = "https://cloud-images.ubuntu.com/releases/mantic/release-20240209/ubuntu-23.10-server-cloudimg-amd64.img"
  }
}

variable "base_images_checksum" {
  type = map(string)
  default = {
    "focal" = "9dfe9ba2f0c16fc7b6e0aa36dda6f201fdd2e64985980aad892115d902545c73"
    "jammy" = "c5eed826009c9f671bc5f7c9d5d63861aa2afe91aeff1c0d3a4cb5b28b2e35d6"
    "mantic" = "2f9d9925e4784e582e34b108c94e6ffe07323c703185bab315ee57e0fc40f67b"
  }
}

variable "default_mac_address" {
  type = map(string)
  default = {
    "5.4" = "52:54:05:04:31:80"
    "5.15" = "52:54:05:15:31:80"
    "6.1" = "52:54:06:01:31:80"
    "6.6" = "52:54:06:06:31:80"
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