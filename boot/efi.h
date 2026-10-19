/*
 * efi.h - the small slice of the UEFI specification this bootloader uses.
 *
 * Written from the UEFI 2.x spec so the project has no external dependencies
 * (no gnu-efi, no EDK2). Table layouts must match the spec exactly; unused
 * function slots are kept as void* placeholders so offsets line up.
 */
#pragma once
#include <stddef.h>
#include <stdint.h>

typedef uint64_t UINTN;
typedef uint64_t EFI_STATUS;
typedef void *EFI_HANDLE;
typedef void *EFI_EVENT;
typedef uint16_t CHAR16;
typedef uint64_t EFI_PHYSICAL_ADDRESS;

#define EFIAPI __attribute__((ms_abi))
#define EFI_SUCCESS 0
#define EFI_ERROR_BIT (1ull << 63)
#define EFI_ERROR(s) (((s) & EFI_ERROR_BIT) != 0)
#define EFI_LOAD_ERROR (EFI_ERROR_BIT | 1)
#define EFI_INVALID_PARAMETER (EFI_ERROR_BIT | 2)
#define EFI_UNSUPPORTED (EFI_ERROR_BIT | 3)
#define EFI_BUFFER_TOO_SMALL (EFI_ERROR_BIT | 5)
#define EFI_NOT_FOUND (EFI_ERROR_BIT | 14)

typedef struct {
    uint32_t Data1;
    uint16_t Data2;
    uint16_t Data3;
    uint8_t Data4[8];
} EFI_GUID;

typedef struct {
    uint64_t Signature;
    uint32_t Revision;
    uint32_t HeaderSize;
    uint32_t CRC32;
    uint32_t Reserved;
} EFI_TABLE_HEADER;

/* ---- Console output ---------------------------------------------------- */

typedef struct EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL;
struct EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL {
    void *Reset;
    EFI_STATUS(EFIAPI *OutputString)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *, const CHAR16 *);
    void *TestString;
    void *QueryMode;
    void *SetMode;
    void *SetAttribute;
    EFI_STATUS(EFIAPI *ClearScreen)(EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *);
    void *SetCursorPosition;
    void *EnableCursor;
    void *Mode;
};

/* ---- Memory ------------------------------------------------------------ */

typedef enum {
    AllocateAnyPages,
    AllocateMaxAddress,
    AllocateAddress,
} EFI_ALLOCATE_TYPE;

typedef enum {
    EfiReservedMemoryType,
    EfiLoaderCode,
    EfiLoaderData,
    EfiBootServicesCode,
    EfiBootServicesData,
    EfiRuntimeServicesCode,
    EfiRuntimeServicesData,
    EfiConventionalMemory,
} EFI_MEMORY_TYPE;

typedef struct {
    uint32_t Type;
    EFI_PHYSICAL_ADDRESS PhysicalStart;
    uint64_t VirtualStart;
    uint64_t NumberOfPages;
    uint64_t Attribute;
} EFI_MEMORY_DESCRIPTOR;

/* ---- Boot services ----------------------------------------------------- */

typedef struct {
    EFI_TABLE_HEADER Hdr;
    void *RaiseTPL;
    void *RestoreTPL;
    EFI_STATUS(EFIAPI *AllocatePages)(EFI_ALLOCATE_TYPE, EFI_MEMORY_TYPE, UINTN, EFI_PHYSICAL_ADDRESS *);
    EFI_STATUS(EFIAPI *FreePages)(EFI_PHYSICAL_ADDRESS, UINTN);
    EFI_STATUS(EFIAPI *GetMemoryMap)(UINTN *, EFI_MEMORY_DESCRIPTOR *, UINTN *, UINTN *, uint32_t *);
    EFI_STATUS(EFIAPI *AllocatePool)(EFI_MEMORY_TYPE, UINTN, void **);
    EFI_STATUS(EFIAPI *FreePool)(void *);
    void *CreateEvent;
    void *SetTimer;
    void *WaitForEvent;
    void *SignalEvent;
    void *CloseEvent;
    void *CheckEvent;
    void *InstallProtocolInterface;
    void *ReinstallProtocolInterface;
    void *UninstallProtocolInterface;
    EFI_STATUS(EFIAPI *HandleProtocol)(EFI_HANDLE, const EFI_GUID *, void **);
    void *Reserved;
    void *RegisterProtocolNotify;
    void *LocateHandle;
    void *LocateDevicePath;
    void *InstallConfigurationTable;
    void *LoadImage;
    void *StartImage;
    void *Exit;
    void *UnloadImage;
    EFI_STATUS(EFIAPI *ExitBootServices)(EFI_HANDLE, UINTN);
    void *GetNextMonotonicCount;
    EFI_STATUS(EFIAPI *Stall)(UINTN);
    EFI_STATUS(EFIAPI *SetWatchdogTimer)(UINTN, uint64_t, UINTN, const CHAR16 *);
    void *ConnectController;
    void *DisconnectController;
    void *OpenProtocol;
    void *CloseProtocol;
    void *OpenProtocolInformation;
    void *ProtocolsPerHandle;
    EFI_STATUS(EFIAPI *LocateHandleBuffer)(int search_type, const EFI_GUID *, void *key, UINTN *count, EFI_HANDLE **buf);
    EFI_STATUS(EFIAPI *LocateProtocol)(const EFI_GUID *, void *, void **);
    void *InstallMultipleProtocolInterfaces;
    void *UninstallMultipleProtocolInterfaces;
    void *CalculateCrc32;
    void *CopyMem;
    void *SetMem;
    void *CreateEventEx;
} EFI_BOOT_SERVICES;

/* ---- System table ------------------------------------------------------ */

typedef struct {
    EFI_GUID VendorGuid;
    void *VendorTable;
} EFI_CONFIGURATION_TABLE;

typedef struct {
    EFI_TABLE_HEADER Hdr;
    CHAR16 *FirmwareVendor;
    uint32_t FirmwareRevision;
    EFI_HANDLE ConsoleInHandle;
    void *ConIn;
    EFI_HANDLE ConsoleOutHandle;
    EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *ConOut;
    EFI_HANDLE StandardErrorHandle;
    EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *StdErr;
    void *RuntimeServices;
    EFI_BOOT_SERVICES *BootServices;
    UINTN NumberOfTableEntries;
    EFI_CONFIGURATION_TABLE *ConfigurationTable;
} EFI_SYSTEM_TABLE;

/* ---- Loaded image ------------------------------------------------------ */

#define EFI_LOADED_IMAGE_PROTOCOL_GUID \
    {0x5B1B31A1, 0x9562, 0x11d2, {0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B}}

#define EFI_DEVICE_PATH_PROTOCOL_GUID \
    {0x09576e91, 0x6d3f, 0x11d2, {0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b}}
#define EFI_GLOBAL_VARIABLE_GUID \
    {0x8be4df61, 0x93ca, 0x11d2, {0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c}}
#define EFI_VARIABLE_NON_VOLATILE 0x1
#define EFI_VARIABLE_BOOTSERVICE_ACCESS 0x2
#define EFI_VARIABLE_RUNTIME_ACCESS 0x4
#define EFI_BOOT_OPTION_ACTIVE 0x1

/* ---- Runtime services (only the slots this loader uses) ---------------- */

typedef struct {
    EFI_TABLE_HEADER Hdr;
    void *GetTime;
    void *SetTime;
    void *GetWakeupTime;
    void *SetWakeupTime;
    void *SetVirtualAddressMap;
    void *ConvertPointer;
    EFI_STATUS(EFIAPI *GetVariable)(const CHAR16 *, const EFI_GUID *, uint32_t *, UINTN *, void *);
    void *GetNextVariableName;
    EFI_STATUS(EFIAPI *SetVariable)(const CHAR16 *, const EFI_GUID *, uint32_t, UINTN, const void *);
} EFI_RUNTIME_SERVICES;

typedef struct {
    uint32_t Revision;
    EFI_HANDLE ParentHandle;
    EFI_SYSTEM_TABLE *SystemTable;
    EFI_HANDLE DeviceHandle;
    void *FilePath;
    void *Reserved;
    uint32_t LoadOptionsSize;
    void *LoadOptions;
    void *ImageBase;
    uint64_t ImageSize;
    EFI_MEMORY_TYPE ImageCodeType;
    EFI_MEMORY_TYPE ImageDataType;
    void *Unload;
} EFI_LOADED_IMAGE_PROTOCOL;

/* ---- File system ------------------------------------------------------- */

/* ---- Block I/O: raw sectors of a disk or CD ----------------------------- */

#define EFI_BLOCK_IO_PROTOCOL_GUID \
    {0x964e5b21, 0x6459, 0x11d2, {0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b}}
#define EFI_LOCATE_BY_PROTOCOL 2

typedef struct {
    uint32_t MediaId;
    uint8_t RemovableMedia, MediaPresent, LogicalPartition, ReadOnly, WriteCaching;
    uint32_t BlockSize, IoAlign;
    uint64_t LastBlock;
} EFI_BLOCK_IO_MEDIA;

typedef struct EFI_BLOCK_IO_PROTOCOL {
    uint64_t Revision;
    EFI_BLOCK_IO_MEDIA *Media;
    void *Reset;
    EFI_STATUS(EFIAPI *ReadBlocks)(struct EFI_BLOCK_IO_PROTOCOL *, uint32_t media_id, uint64_t lba, UINTN size, void *buf);
    EFI_STATUS(EFIAPI *WriteBlocks)(struct EFI_BLOCK_IO_PROTOCOL *, uint32_t media_id, uint64_t lba, UINTN size,
                                    const void *buf);
    EFI_STATUS(EFIAPI *FlushBlocks)(struct EFI_BLOCK_IO_PROTOCOL *);
} EFI_BLOCK_IO_PROTOCOL;

#define EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID \
    {0x964E5B22, 0x6459, 0x11d2, {0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B}}

#define EFI_FILE_MODE_READ 0x1ull

typedef struct EFI_FILE_PROTOCOL EFI_FILE_PROTOCOL;
struct EFI_FILE_PROTOCOL {
    uint64_t Revision;
    EFI_STATUS(EFIAPI *Open)(EFI_FILE_PROTOCOL *, EFI_FILE_PROTOCOL **, const CHAR16 *, uint64_t, uint64_t);
    EFI_STATUS(EFIAPI *Close)(EFI_FILE_PROTOCOL *);
    void *Delete;
    EFI_STATUS(EFIAPI *Read)(EFI_FILE_PROTOCOL *, UINTN *, void *);
    void *Write;
    EFI_STATUS(EFIAPI *GetPosition)(EFI_FILE_PROTOCOL *, uint64_t *);
    EFI_STATUS(EFIAPI *SetPosition)(EFI_FILE_PROTOCOL *, uint64_t);
    void *GetInfo;
    void *SetInfo;
    void *Flush;
};

typedef struct EFI_SIMPLE_FILE_SYSTEM_PROTOCOL EFI_SIMPLE_FILE_SYSTEM_PROTOCOL;
struct EFI_SIMPLE_FILE_SYSTEM_PROTOCOL {
    uint64_t Revision;
    EFI_STATUS(EFIAPI *OpenVolume)(EFI_SIMPLE_FILE_SYSTEM_PROTOCOL *, EFI_FILE_PROTOCOL **);
};

/* ---- Graphics Output Protocol (framebuffer) ---------------------------- */

#define EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID \
    {0x9042a9de, 0x23dc, 0x4a38, {0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a}}

typedef enum {
    PixelRedGreenBlueReserved8BitPerColor,
    PixelBlueGreenRedReserved8BitPerColor,
    PixelBitMask,
    PixelBltOnly,
} EFI_GRAPHICS_PIXEL_FORMAT;

typedef struct {
    uint32_t RedMask, GreenMask, BlueMask, ReservedMask;
} EFI_PIXEL_BITMASK;

typedef struct {
    uint32_t Version;
    uint32_t HorizontalResolution;
    uint32_t VerticalResolution;
    EFI_GRAPHICS_PIXEL_FORMAT PixelFormat;
    EFI_PIXEL_BITMASK PixelInformation;
    uint32_t PixelsPerScanLine;
} EFI_GRAPHICS_OUTPUT_MODE_INFORMATION;

typedef struct {
    uint32_t MaxMode;
    uint32_t Mode;
    EFI_GRAPHICS_OUTPUT_MODE_INFORMATION *Info;
    UINTN SizeOfInfo;
    EFI_PHYSICAL_ADDRESS FrameBufferBase;
    UINTN FrameBufferSize;
} EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE;

typedef struct EFI_GRAPHICS_OUTPUT_PROTOCOL EFI_GRAPHICS_OUTPUT_PROTOCOL;
struct EFI_GRAPHICS_OUTPUT_PROTOCOL {
    EFI_STATUS(EFIAPI *QueryMode)(EFI_GRAPHICS_OUTPUT_PROTOCOL *, uint32_t, UINTN *,
                                  EFI_GRAPHICS_OUTPUT_MODE_INFORMATION **);
    EFI_STATUS(EFIAPI *SetMode)(EFI_GRAPHICS_OUTPUT_PROTOCOL *, uint32_t);
    void *Blt;
    EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE *Mode;
};

/* ---- Configuration tables ---------------------------------------------- */

#define EFI_ACPI_20_TABLE_GUID \
    {0x8868e871, 0xe4f1, 0x11d3, {0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81}}
