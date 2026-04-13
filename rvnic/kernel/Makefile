# Makefile for Rattan Virtual NIC kernel module

obj-m += rattan_vnic.o

KDIR ?= /lib/modules/$(shell uname -r)/build
PWD := $(shell pwd)

# Default target
all:
	$(MAKE) -C $(KDIR) M=$(PWD) modules

clean:
	$(MAKE) -C $(KDIR) M=$(PWD) clean
	rm -f Module.symvers modules.order

install:
	$(MAKE) -C $(KDIR) M=$(PWD) modules_install
	depmod -a

uninstall:
	rm -f /lib/modules/$(shell uname -r)/extra/rattan_vnic.ko
	depmod -a

load:
	sudo insmod rattan_vnic.ko

unload:
	sudo rmmod rattan_vnic

reload: unload load

# Generate compile_commands.json for IDE/LSP support
compile_commands.json:
	$(MAKE) -C $(KDIR) M=$(PWD) compile_commands.json

# Development helpers
check:
	@echo "Checking module info..."
	@modinfo rattan_vnic.ko 2>/dev/null || echo "Module not built"

.PHONY: all clean install uninstall load unload reload check
