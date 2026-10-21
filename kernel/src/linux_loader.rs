// SPDX-License-Identifier: Apache-2.0
//! Linux dynamic-image loading.
//!
//! Maps a position-independent main image and its requested interpreter into a
//! fresh address space, builds the initial process stack with argv/envp/auxv,
//! and enters the interpreter. The interpreter is responsible for relocations
//! and shared-library loading; the kernel only supplies the mapped images and
//! the ABI stack.
//!
//! The caller resolves the `PT_INTERP` path through the verified runtime rootfs
//! and supplies the interpreter bytes; the loader validates that the main image
//! names an interpreter and maps both images.

use crate::memory::paging;
use aether_abi::user::{USER_BASE, USER_LIMIT, USER_STACK_TOP};
use aether_linux_elf::{Image, ParseError, Segment};
use aether_memory::PAGE_SIZE;
use aether_memory::frame::{FrameAllocator, PhysFrame};
use aether_memory::paging::PageFlags;
use core::ptr;

/// Load bias for the main position-independent image.
pub const MAIN_BASE: u64 = USER_BASE + 80 * 1024 * 1024;
/// Load bias for the interpreter.
///
/// The GridCraft GUI's main image is ~34 MiB. It starts at `MAIN_BASE`
/// (80 MiB), so a 96 MiB interpreter base would overlap it and corrupt the
/// interpreter's GOT. Keep the interpreter above the largest accepted main
/// image; the 128 MiB user window still leaves room for the stack.
pub const INTERP_BASE: u64 = USER_BASE + 120 * 1024 * 1024;

/// A loaded Linux process image.
#[derive(Clone, Copy, Debug)]
pub struct Loaded {
    /// New address space.
    pub pml4: PhysFrame,
    /// Interpreter entry point.
    pub entry: u64,
    /// Initial user stack pointer.
    pub stack_pointer: u64,
}

/// Loader failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// Main image parse failure.
    Main(ParseError),
    /// Interpreter parse failure.
    Interpreter(ParseError),
    /// The main image did not request the expected interpreter.
    NoInterpreter,
    /// A segment or the stack could not be mapped.
    Paging,
    /// The image does not fit the user window.
    OutsideWindow,
    /// The stack could not be represented.
    Stack,
}

impl From<paging::PagingError> for LoadError {
    fn from(_: paging::PagingError) -> Self {
        Self::Paging
    }
}

/// Loads `main` and its resolved `interpreter`.
///
/// `executable_path` is the path the application was launched as; it becomes
/// `argv[0]` and `AT_EXECFN`. The interpreter path itself is taken from the
/// main image's `PT_INTERP` entry and must have been used to resolve
/// `interpreter`.
pub fn load(
    allocator: &mut FrameAllocator<'_>,
    main: &[u8],
    interpreter: &[u8],
    executable_path: &str,
    arguments: &[&str],
    environment: &[&str],
) -> Result<Loaded, LoadError> {
    if executable_path.len() + 1 > 256 {
        return Err(LoadError::Stack);
    }
    let main = aether_linux_elf::parse(main).map_err(LoadError::Main)?;
    let interp = aether_linux_elf::parse(interpreter).map_err(LoadError::Interpreter)?;
    if main.interpreter().is_none() {
        return Err(LoadError::NoInterpreter);
    }

    let main_bias = bias(&main, MAIN_BASE);
    let interp_bias = bias(&interp, INTERP_BASE);
    check_window(&main, main_bias)?;
    check_window(&interp, interp_bias)?;

    let pml4 = paging::create_empty_user_address_space(allocator)?;
    paging::map_user_stack(allocator, pml4)?;
    for segment in main.segments() {
        map_segment(allocator, pml4, *segment, main_bias)?;
    }
    for segment in interp.segments() {
        map_segment(allocator, pml4, *segment, interp_bias)?;
    }

    let old_pml4 = paging::current_pml4().ok_or(LoadError::Paging)?;
    paging::activate(pml4);
    let result = write_images_and_stack(
        pml4,
        &main,
        main_bias,
        &interp,
        interp_bias,
        executable_path,
        arguments,
        environment,
    );
    paging::activate(old_pml4);
    let stack_pointer = result?;

    let interp_entry = interp_bias
        .checked_add(interp.entry())
        .ok_or(LoadError::OutsideWindow)?;
    Ok(Loaded {
        pml4,
        entry: interp_entry,
        stack_pointer,
    })
}

fn bias(image: &Image<'_>, base: u64) -> u64 {
    if image.image_type() == aether_linux_elf::ET_DYN {
        base
    } else {
        0
    }
}

fn check_window(image: &Image<'_>, bias: u64) -> Result<(), LoadError> {
    for segment in image.segments() {
        let start = bias
            .checked_add(segment.vaddr)
            .ok_or(LoadError::OutsideWindow)?;
        let end = start
            .checked_add(segment.memory_size)
            .ok_or(LoadError::OutsideWindow)?;
        if start < USER_BASE || end > USER_LIMIT - 16 * PAGE_SIZE {
            return Err(LoadError::OutsideWindow);
        }
    }
    Ok(())
}

fn map_segment(
    allocator: &mut FrameAllocator<'_>,
    pml4: PhysFrame,
    segment: Segment,
    bias: u64,
) -> Result<(), LoadError> {
    let start = bias
        .checked_add(segment.vaddr)
        .ok_or(LoadError::OutsideWindow)?;
    let end = start
        .checked_add(segment.memory_size)
        .ok_or(LoadError::OutsideWindow)?;
    let page_start = start & !(PAGE_SIZE - 1);
    let page_end = end
        .checked_add(PAGE_SIZE - 1)
        .map(|value| value & !(PAGE_SIZE - 1))
        .ok_or(LoadError::OutsideWindow)?;
    // Map writable and non-executable while the kernel copies file data.
    paging::map_allocated_user_range(
        allocator,
        pml4,
        page_start,
        page_end - page_start,
        PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE | PageFlags::NO_EXECUTE,
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_images_and_stack(
    pml4: PhysFrame,
    main: &Image<'_>,
    main_bias: u64,
    interp: &Image<'_>,
    interp_bias: u64,
    executable_path: &str,
    arguments: &[&str],
    environment: &[&str],
) -> Result<u64, LoadError> {
    copy_image(main, main_bias)?;
    copy_image(interp, interp_bias)?;
    for segment in main.segments() {
        protect(pml4, main_bias, *segment)?;
    }
    for segment in interp.segments() {
        protect(pml4, interp_bias, *segment)?;
    }
    build_stack(
        main,
        main_bias,
        interp_bias,
        executable_path,
        arguments,
        environment,
    )
}

fn copy_image(image: &Image<'_>, bias: u64) -> Result<(), LoadError> {
    for segment in image.segments() {
        if segment.file_size == 0 {
            continue;
        }
        let start = bias
            .checked_add(segment.vaddr)
            .ok_or(LoadError::OutsideWindow)?;
        let source_start = usize::try_from(segment.offset).map_err(|_| LoadError::Paging)?;
        let source_end = source_start
            .checked_add(segment.file_size as usize)
            .ok_or(LoadError::Paging)?;
        let source = image
            .bytes()
            .get(source_start..source_end)
            .ok_or(LoadError::Paging)?;
        // SAFETY: The destination range was mapped writable above and lies in
        // the fresh address space. Interrupts are disabled during boot and the
        // source is a validated slice of the embedded image.
        unsafe {
            ptr::copy_nonoverlapping(source.as_ptr(), start as *mut u8, source.len());
        }
    }
    Ok(())
}

fn protect(pml4: PhysFrame, bias: u64, segment: Segment) -> Result<(), LoadError> {
    let start = bias
        .checked_add(segment.vaddr)
        .ok_or(LoadError::OutsideWindow)?;
    let end = start
        .checked_add(segment.memory_size)
        .ok_or(LoadError::OutsideWindow)?;
    let page_start = start & !(PAGE_SIZE - 1);
    let page_end = end
        .checked_add(PAGE_SIZE - 1)
        .map(|value| value & !(PAGE_SIZE - 1))
        .ok_or(LoadError::OutsideWindow)?;
    let mut flags = PageFlags::PRESENT | PageFlags::USER;
    if segment.writable() {
        flags = flags | PageFlags::WRITABLE;
    }
    if !segment.executable() {
        flags = flags | PageFlags::NO_EXECUTE;
    }
    paging::protect_user_range(pml4, page_start, page_end - page_start, flags)?;
    Ok(())
}

fn build_stack(
    main: &Image<'_>,
    main_bias: u64,
    interp_bias: u64,
    executable_path: &str,
    arguments: &[&str],
    environment: &[&str],
) -> Result<u64, LoadError> {
    const STACK_PAGE_BASE: u64 = USER_STACK_TOP - PAGE_SIZE;
    // The vector area sits at the initial RSP and grows upward; the string
    // area must sit ABOVE it because the user stack itself grows DOWN from RSP
    // and would otherwise overwrite argv/envp strings during startup.
    const VECTOR_BASE: usize = 1024;
    const STRING_BASE: usize = 2048;
    let mut page = [0u8; PAGE_SIZE as usize];

    if arguments.is_empty() || arguments.len() > 16 || environment.len() > 16 {
        return Err(LoadError::Stack);
    }
    let mut string_cursor = STRING_BASE;
    let mut put_string = |value: &str| -> Result<u64, LoadError> {
        let length = value.len().checked_add(1).ok_or(LoadError::Stack)?;
        let end = string_cursor.checked_add(length).ok_or(LoadError::Stack)?;
        if end > page.len() {
            return Err(LoadError::Stack);
        }
        let pointer = STACK_PAGE_BASE + string_cursor as u64;
        page[string_cursor..string_cursor + value.len()].copy_from_slice(value.as_bytes());
        page[string_cursor + value.len()] = 0;
        string_cursor = end;
        Ok(pointer)
    };
    let mut argv = [0u64; 16];
    for (slot, value) in argv.iter_mut().zip(arguments) {
        *slot = put_string(value)?;
    }
    let mut envp = [0u64; 16];
    for (slot, value) in envp.iter_mut().zip(environment) {
        *slot = put_string(value)?;
    }
    let path_pointer = put_string(executable_path)?;
    let platform_pointer = put_string("x86_64")?;
    string_cursor = string_cursor.checked_add(16).ok_or(LoadError::Stack)?;
    if string_cursor
        .checked_add(16)
        .is_none_or(|end| end > page.len())
    {
        return Err(LoadError::Stack);
    }
    let random_pointer = STACK_PAGE_BASE + string_cursor as u64;
    for (index, byte) in page[string_cursor..string_cursor + 16]
        .iter_mut()
        .enumerate()
    {
        *byte = (index as u8).wrapping_mul(37).wrapping_add(11);
    }
    string_cursor += 16;

    // Keep the vector/auxv area below the strings and align the entry stack.
    let mut offset = VECTOR_BASE;
    let mut push = |value: u64| -> Result<(), LoadError> {
        let end = offset.checked_add(8).ok_or(LoadError::Stack)?;
        if end > page.len() {
            return Err(LoadError::Stack);
        }
        page[offset..end].copy_from_slice(&value.to_le_bytes());
        offset = end;
        Ok(())
    };

    push(arguments.len() as u64)?; // argc
    for pointer in argv.iter().take(arguments.len()) {
        push(*pointer)?;
    }
    push(0)?; // argv terminator
    for pointer in envp.iter().take(environment.len()) {
        push(*pointer)?;
    }
    push(0)?; // envp terminator
    let auxv = [
        (3u64, main.program_header_address(main_bias)), // AT_PHDR
        (4, main.program_header_size() as u64),         // AT_PHENT
        (5, main.program_header_count() as u64),        // AT_PHNUM
        (6, PAGE_SIZE),                                 // AT_PAGESZ
        (7, interp_bias),                               // AT_BASE
        (8, 0),                                         // AT_FLAGS
        (9, main_bias + main.entry()),                  // AT_ENTRY
        (11, 1000),                                     // AT_UID
        (12, 1000),                                     // AT_EUID
        (13, 1000),                                     // AT_GID
        (14, 1000),                                     // AT_EGID
        (15, platform_pointer),                         // AT_PLATFORM
        (16, 0),                                        // AT_HWCAP
        (17, 100),                                      // AT_CLKTCK
        (23, 0),                                        // AT_SECURE
        (25, random_pointer),                           // AT_RANDOM
        (31, path_pointer),                             // AT_EXECFN
        (0, 0),                                         // AT_NULL
    ];
    for (key, value) in auxv {
        push(key)?;
        push(value)?;
    }

    // Strings live above the vector area, so copy through whichever region
    // ends last rather than just through the auxv table.
    let copied = offset.max(string_cursor);
    // SAFETY: The stack page was mapped writable in the active address space;
    // all strings and vectors are within the copied prefix.
    unsafe {
        ptr::copy_nonoverlapping(page.as_ptr(), STACK_PAGE_BASE as *mut u8, copied);
    }
    Ok(STACK_PAGE_BASE + VECTOR_BASE as u64)
}
