// Send bounded key holds to one Gamescope EIS socket, preserving the device until release.
#include <libei.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static struct ei *context;
static struct ei_device *keyboard;
static bool ready;
static bool disconnected;
static volatile sig_atomic_t interrupted;

static void stop(int signal_number) { (void)signal_number; interrupted = 1; }

static void dispatch(void) {
    ei_dispatch(context);
    struct ei_event *event;
    while ((event = ei_get_event(context))) {
        enum ei_event_type type = ei_event_get_type(event);
        struct ei_device *device = ei_event_get_device(event);
        if (type == EI_EVENT_SEAT_ADDED) {
            ei_seat_bind_capabilities(ei_event_get_seat(event), EI_DEVICE_CAP_KEYBOARD,
                                     EI_DEVICE_CAP_POINTER, EI_DEVICE_CAP_BUTTON, NULL);
        } else if (type == EI_EVENT_DEVICE_RESUMED &&
                   ei_device_has_capability(device, EI_DEVICE_CAP_KEYBOARD)) {
            if (keyboard) ei_device_unref(keyboard);
            keyboard = ei_device_ref(device);
            ready = true;
        } else if ((type == EI_EVENT_DEVICE_PAUSED || type == EI_EVENT_DEVICE_REMOVED) &&
                   device == keyboard) {
            ready = false;
        } else if (type == EI_EVENT_DISCONNECT) {
            disconnected = true;
        }
        ei_event_unref(event);
    }
}

static void pump(unsigned milliseconds) {
    uint64_t deadline = ei_now(context) + (uint64_t)milliseconds * 1000;
    do {
        struct pollfd fd = {.fd = ei_get_fd(context), .events = POLLIN};
        poll(&fd, 1, 5);
        dispatch();
    } while (!interrupted && !disconnected && ei_now(context) < deadline);
}

static void send_key(unsigned code, bool pressed) {
    ei_device_keyboard_key(keyboard, code, pressed);
    uint64_t now = ei_now(context);
    ei_device_frame(keyboard, now);
    printf("{\"key\":%u,\"down\":%s,\"host_monotonic_us\":%llu}\n",
           code, pressed ? "true" : "false", (unsigned long long)now);
    fflush(stdout);
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: input EIS_SOCKET KEY:HELD_MS|click:X:Y [...]\n");
        return 2;
    }
    // Validate the entire sequence before sending any event.
    for (int i = 2; i < argc; ++i) {
        unsigned code, duration, second;
        char extra;
        if (sscanf(argv[i], "click:%u:%u%c", &code, &duration, &extra) == 2) {
            if (code > 4000 || duration > 4000) return 2;
            continue;
        }
        if (sscanf(argv[i], "%u+%u:%u%c", &code, &second, &duration, &extra) == 3) {
            if (code > 255 || second > 255 || code == second || duration == 0 || duration > 10000) return 2;
        } else if (sscanf(argv[i], "%u:%u%c", &code, &duration, &extra) != 2 ||
                   code > 255 || duration == 0 || duration > 10000) return 2;
    }
    signal(SIGINT, stop);
    signal(SIGTERM, stop);
    context = ei_new_sender(NULL);
    if (!context) return 3;
    ei_configure_name(context, "mudcrab-motion-recorder");
    if (ei_setup_backend_socket(context, argv[1]) != 0) return 3;
    uint64_t deadline = ei_now(context) + 5000000;
    while (!ready && !interrupted && !disconnected && ei_now(context) < deadline) pump(10);
    if (!ready || disconnected || interrupted) {
        fprintf(stderr, "No resumed keyboard on the owned EIS socket\n");
        ei_unref(context);
        return 4;
    }
    ei_device_start_emulating(keyboard, 1);
    int result = 0;
    for (int i = 2; i < argc; ++i) {
        unsigned code, duration;
        if (!ready || disconnected || interrupted) { result = 5; break; }
        if (sscanf(argv[i], "click:%u:%u", &code, &duration) == 2) {
            if (!ei_device_has_capability(keyboard, EI_DEVICE_CAP_POINTER) ||
                !ei_device_has_capability(keyboard, EI_DEVICE_CAP_BUTTON)) { result = 6; break; }
            // Skyrim reads relative mouse input even in menus. Clamp to a known corner.
            for (int step = 0; step < 8 && ready && !interrupted; ++step) {
                ei_device_pointer_motion(keyboard, -1000, -1000);
                ei_device_frame(keyboard, ei_now(context));
                pump(25);
            }
            for (int step = 0; step < 4 && ready && !interrupted; ++step) {
                ei_device_pointer_motion(keyboard, code / 4.0, duration / 4.0);
                ei_device_frame(keyboard, ei_now(context));
                pump(25);
            }
            pump(200);
            if (!ready || interrupted || disconnected) { result = 5; break; }
            ei_device_button_button(keyboard, 272, true);
            ei_device_frame(keyboard, ei_now(context));
            printf("{\"click\":[%u,%u],\"down\":true,\"host_monotonic_us\":%llu}\n",
                   code, duration, (unsigned long long)ei_now(context));
            pump(200);
            if (ready && !disconnected) {
                ei_device_button_button(keyboard, 272, false);
                ei_device_frame(keyboard, ei_now(context));
                printf("{\"click\":[%u,%u],\"down\":false,\"host_monotonic_us\":%llu}\n",
                       code, duration, (unsigned long long)ei_now(context));
            }
            pump(200);
            if (!ready || disconnected || interrupted) { result = 5; break; }
            continue;
        }
        unsigned second = 0;
        bool chord = sscanf(argv[i], "%u+%u:%u", &code, &second, &duration) == 3;
        if (!chord) sscanf(argv[i], "%u:%u", &code, &duration);
        if (chord) send_key(second, true);
        send_key(code, true);
        pump(duration);
        // Keep the connection alive through release, including SIGINT/SIGTERM cleanup.
        if (ready && !disconnected) {
            send_key(code, false);
            if (chord) send_key(second, false);
        }
        if (!ready || disconnected || interrupted) { result = 5; break; }
        pump(100);
    }
    if (ready && !disconnected) {
        ei_device_stop_emulating(keyboard);
        pump(100);
    }
    ei_device_unref(keyboard);
    ei_unref(context);
    return result;
}
