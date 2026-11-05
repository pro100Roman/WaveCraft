#include <stddef.h>

/*
 * libhackrf checks for a kernel driver and asks libusb to detach it before
 * claiming interface 0. On recent macOS releases, detaching captures the
 * entire USB device and therefore requires root or a restricted entitlement.
 * HackRF One uses a vendor-specific interface that can be claimed directly,
 * so let libhackrf continue to libusb_claim_interface().
 *
 * Keep this interposer scoped to the dedicated macOS launcher. It is not
 * linked into normal WaveCraft builds and does not modify Homebrew libraries.
 */
static int wavecraft_libusb_detach_kernel_driver(void *device_handle,
                                                  int interface_number)
{
    (void)device_handle;
    (void)interface_number;
    return 0;
}

#define DYLD_INTERPOSE(replacement, replacee)                                  \
    __attribute__((used)) static struct {                                      \
        const void *replacement;                                               \
        const void *replacee;                                                  \
    } _interpose_##replacee __attribute__((section("__DATA,__interpose"))) = { \
        (const void *)(unsigned long)&replacement,                             \
        (const void *)(unsigned long)&replacee                                 \
    }

extern int libusb_detach_kernel_driver(void *device_handle,
                                       int interface_number);
DYLD_INTERPOSE(wavecraft_libusb_detach_kernel_driver,
               libusb_detach_kernel_driver);
