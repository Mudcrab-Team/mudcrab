#include <cstddef>
#include <cstdint>
#include <cstdio>

#if defined(_WIN32)
#include <fcntl.h>
#include <io.h>
#include <windows.h>
#endif

// basis-universal-rs 0.3 compiles these stable low-level encoder entry points,
// but does not expose them from its Rust API. Keeping the bridge header-free
// avoids vendoring the codec and leaves ownership with the upstream crate.
namespace basisu {
struct image_stats;
void* basis_compress(const std::uint8_t* rgba, std::uint32_t width,
                     std::uint32_t height, std::uint32_t pitch_in_pixels,
                     std::uint32_t flags_and_quality, float uastc_rdo_quality,
                     std::size_t* size, image_stats* stats);
void basis_free_data(void* data);
}

extern "C" void* opensky_basis_compress_ktx2(
    const std::uint8_t* rgba, std::uint32_t width, std::uint32_t height,
    std::uint32_t flags_and_quality, float uastc_rdo_quality,
    std::size_t* size) {
    return basisu::basis_compress(rgba, width, height, width,
                                  flags_and_quality, uastc_rdo_quality,
                                  size, nullptr);
}

extern "C" void opensky_basis_free(void* data) {
    basisu::basis_free_data(data);
}

// The encoder prints "WARNING: Due to a KTX2 validator bug related to
// mipPadding, ..." with a plain printf each time it pads a KTX2 file's
// key/value data, and no option turns it off. On a full conversion that is
// thousands of lines through the converter's progress output. Only C and C++
// code prints through the C library's stdout; Rust writes to the process's
// standard output directly. So the C library's stdout is pointed at the null
// device and Rust's output is left where it was.
extern "C" void opensky_basis_quiet_stdout() {
    std::fflush(stdout);
#if defined(_WIN32)
    // The C runtime closes the old handle of descriptor 1 and reports the new
    // one to the process through SetStdHandle, so keep a duplicate of the
    // process's standard output and restore it afterwards.
    HANDLE process = GetCurrentProcess();
    HANDLE original = GetStdHandle(STD_OUTPUT_HANDLE);
    HANDLE kept = nullptr;
    bool has_output = original != nullptr && original != INVALID_HANDLE_VALUE;
    if (has_output &&
        !DuplicateHandle(process, original, process, &kept, 0, FALSE,
                         DUPLICATE_SAME_ACCESS)) {
        return;
    }
    int null_device = _open("NUL", _O_WRONLY);
    if (null_device >= 0) {
        _dup2(null_device, _fileno(stdout));
        _close(null_device);
    }
    if (has_output) {
        SetStdHandle(STD_OUTPUT_HANDLE, kept);
    }
#elif defined(__GLIBC__) || defined(__APPLE__)
    // Rust writes to descriptor 1 itself, so the descriptor stays; only the C
    // library's stdout stream is replaced, which both libraries allow.
    if (std::FILE* null_device = std::fopen("/dev/null", "w")) {
        stdout = null_device;
    }
#endif
}
