/* A pointer for the bench's private compositor: zwlr_virtual_pointer_v1.
 *
 *   cc -O2 -o vpointer vpointer.c -lwayland-client
 *   WAYLAND_DISPLAY=<the private compositor's socket> ./vpointer WIDTH HEIGHT
 *
 * Reads commands on stdin, one per line, and answers each with one line:
 *
 *   move X Y     -> "ok <t>"   pointer to (X, Y) of the WIDTHxHEIGHT layout
 *   click X Y    -> "ok <t>"   move there, press and release the left button
 *   quit
 *
 * <t> is CLOCK_MONOTONIC in seconds (Python's time.monotonic()) taken right
 * after the last event was flushed to the compositor: for a click, the
 * release, which is when a GPUI on_click fires.
 *
 * The protocol's interfaces are written out here rather than generated, so
 * nothing but libwayland-client is needed. Signatures follow
 * wlr-virtual-pointer-unstable-v1.xml, version 1.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <wayland-client.h>

extern const struct wl_interface wl_seat_interface;
extern const struct wl_interface wl_output_interface;
const struct wl_interface zwlr_virtual_pointer_v1_interface;

static const struct wl_interface *types[] = {
    NULL, NULL, NULL, NULL, NULL,
    &wl_seat_interface, &zwlr_virtual_pointer_v1_interface,
    &wl_seat_interface, &wl_output_interface, &zwlr_virtual_pointer_v1_interface,
};

static const struct wl_message pointer_requests[] = {
    {"motion", "uff", types + 0},
    {"motion_absolute", "uuuuu", types + 0},
    {"button", "uuu", types + 0},
    {"axis", "uuf", types + 0},
    {"frame", "", types + 0},
    {"axis_source", "u", types + 0},
    {"axis_stop", "uu", types + 0},
    {"axis_discrete", "uufi", types + 0},
    {"destroy", "", types + 0},
};

const struct wl_interface zwlr_virtual_pointer_v1_interface = {
    "zwlr_virtual_pointer_v1", 1, 9, pointer_requests, 0, NULL,
};

static const struct wl_message manager_requests[] = {
    {"create_virtual_pointer", "?on", types + 5},
    {"destroy", "", types + 0},
    {"create_virtual_pointer_with_output", "2?o?on", types + 7},
};

static const struct wl_interface manager_interface = {
    "zwlr_virtual_pointer_manager_v1", 1, 3, manager_requests, 0, NULL,
};

enum { MOTION_ABSOLUTE = 1, BUTTON = 2, FRAME = 4 };
#define BTN_LEFT 0x110

static struct wl_proxy *manager;
static struct wl_seat *seat;

static void global(void *data, struct wl_registry *registry, uint32_t name, const char *interface,
                   uint32_t version) {
    (void)data;
    (void)version;
    if (strcmp(interface, manager_interface.name) == 0)
        manager = wl_registry_bind(registry, name, &manager_interface, 1);
    else if (strcmp(interface, "wl_seat") == 0 && !seat)
        seat = wl_registry_bind(registry, name, &wl_seat_interface, 1);
}

static void global_remove(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data;
    (void)registry;
    (void)name;
}

static const struct wl_registry_listener registry_listener = {global, global_remove};

static uint32_t millis(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (uint32_t)(now.tv_sec * 1000 + now.tv_nsec / 1000000);
}

static double seconds(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec + now.tv_nsec / 1e9;
}

static void pause_ms(long ms) {
    struct timespec span = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&span, NULL);
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: vpointer WIDTH HEIGHT\n");
        return 64;
    }
    uint32_t width = (uint32_t)atoi(argv[1]), height = (uint32_t)atoi(argv[2]);
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) {
        fprintf(stderr, "vpointer: no Wayland display\n");
        return 69;
    }
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &registry_listener, NULL);
    wl_display_roundtrip(display);
    if (!manager) {
        fprintf(stderr, "vpointer: the compositor has no zwlr_virtual_pointer_manager_v1\n");
        return 69;
    }
    struct wl_proxy *pointer = wl_proxy_marshal_flags(manager, 0, &zwlr_virtual_pointer_v1_interface, 1,
                                                      0, seat, NULL);
    wl_display_roundtrip(display);

    char line[256];
    while (fgets(line, sizeof line, stdin)) {
        char verb[16];
        unsigned x = 0, y = 0;
        if (sscanf(line, "%15s %u %u", verb, &x, &y) < 1)
            continue;
        if (strcmp(verb, "quit") == 0)
            break;
        int click = strcmp(verb, "click") == 0;
        if (!click && strcmp(verb, "move") != 0) {
            printf("error unknown command\n");
            fflush(stdout);
            continue;
        }
        wl_proxy_marshal_flags(pointer, MOTION_ABSOLUTE, NULL, 1, 0, millis(), x, y, width, height);
        wl_proxy_marshal_flags(pointer, FRAME, NULL, 1, 0);
        wl_display_roundtrip(display);
        if (click) {
            /* Hover first, as a hand would: the row's hover style settles before the press. */
            pause_ms(60);
            wl_proxy_marshal_flags(pointer, BUTTON, NULL, 1, 0, millis(), BTN_LEFT, 1);
            wl_proxy_marshal_flags(pointer, FRAME, NULL, 1, 0);
            wl_display_flush(display);
            pause_ms(40);
            wl_proxy_marshal_flags(pointer, BUTTON, NULL, 1, 0, millis(), BTN_LEFT, 0);
            wl_proxy_marshal_flags(pointer, FRAME, NULL, 1, 0);
            wl_display_flush(display);
        }
        double stamp = seconds();
        wl_display_roundtrip(display);
        printf("ok %.6f\n", stamp);
        fflush(stdout);
    }
    wl_proxy_marshal_flags(pointer, 8, NULL, 1, WL_MARSHAL_FLAG_DESTROY);
    wl_display_roundtrip(display);
    wl_display_disconnect(display);
    return 0;
}
