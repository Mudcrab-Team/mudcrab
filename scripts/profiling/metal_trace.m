#import <Foundation/Foundation.h>
#import <AppKit/AppKit.h>
#import <Metal/Metal.h>
#import <QuartzCore/QuartzCore.h>
#import <objc/runtime.h>
#include <mach/mach_time.h>
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

// Default observation creates no GPU work/waits; an explicitly requested
// diagnostic display-sync variation changes only the layer presentation policy.
// Callbacks never retain
// a drawable/texture/command buffer in a callback. Build outside the repository.
enum { RING_SIZE = 32768, HOOK_LIMIT = 128, TEXTURE_SLOTS = 256 };
typedef struct {
    char kind[32], name[112], selector[64];
    uint64_t ns, end_ns, thread, thread_cpu_ns, object, queue, device, layer;
    uint64_t cb, acquisition, drawable, texture, created_ns, count, extra;
    double gpu_start, gpu_end, kernel_start, kernel_end, presented, parameter;
    uint32_t status, width, height;
} Event;
typedef struct { Class cls; SEL sel; IMP wrapper; } Hook;
typedef struct { uint64_t texture, acquisition, drawable, layer; } TextureIdentity;
static Event gRing[RING_SIZE];
static Hook gHooks[HOOK_LIMIT];
static TextureIdentity gTextures[TEXTURE_SLOTS];
static size_t gHead, gTail, gHookCount;
static pthread_mutex_t gRingMutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t gHookMutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t gTextureMutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_t gWriter;
static FILE *gFile;
static BOOL gMinimal;
static mach_timebase_info_data_t gTimebase;
static atomic_bool gEnabled, gStop;
static atomic_uint_fast64_t gRows, gDropped, gExceptions, gHookFailures;
static atomic_uint_fast64_t gNextCB, gNextAcquisition, gCommits, gCompleted;
static atomic_uint_fast64_t gScheduled, gPresents, gPresented, gAcquired;
static atomic_uint_fast64_t gTexturesMatched, gTextureMisses, gEmptyAcquire;
static atomic_uint_fast64_t gFactoryCalls, gDuplicateCommit;
static atomic_uint_fast64_t gThreadCPUClockFailures, gCaptureConfigErrors;
static uint64_t gMaxRows = 1000000, gStartAcquisition;
static char kBufferState, kDrawableState, kEncoderState;
static id<MTLDevice> gCaptureDevice;
static NSString *gCapturePath;
static uint64_t gCaptureStart = 1200, gCaptureCount = 3;
static atomic_uint_fast64_t gCaptureTarget;
static atomic_bool gCaptureActive, gCaptureStopRequested;
static atomic_uint_fast64_t gCaptureBeginNS;
static atomic_bool gHUDMenuQueued;
static BOOL inspect_hud_menu(NSMenu *menu, BOOL inReport, NSUInteger depth) {
    id<NSMenuDelegate> delegate = menu.delegate;
    fprintf(stderr, "metal_hud_menu_state: depth=%lu report_context=%d items=%ld delegate=%s\n",
        (unsigned long)depth, inReport, (long)menu.numberOfItems, object_getClassName(delegate));
    if (inReport && menu.numberOfItems == 0) {
        if ([delegate respondsToSelector:@selector(menuNeedsUpdate:)]) [delegate menuNeedsUpdate:menu];
        if ([delegate respondsToSelector:@selector(menuWillOpen:)]) [delegate menuWillOpen:menu];
    }
    [menu update];
    BOOL started = NO;
    for (NSInteger i = 0; i < menu.numberOfItems; i++) {
        NSMenuItem *item = [menu itemAtIndex:i];
        NSString *lower = item.title.lowercaseString;
        BOOL report = inReport || [lower containsString:@"report"];
        fprintf(stderr, "metal_hud_menu: depth=%lu enabled=%d title=%s action=%s report_context=%d\n",
            (unsigned long)depth, item.enabled, item.title.UTF8String,
            item.action ? sel_getName(item.action) : "", report);
        const char *duration = getenv("MUDCRAB_METAL_HUD_REPORT_DURATION");
        NSString *durationTitle = duration ? [NSString stringWithFormat:@"%s seconds", duration] : nil;
        if (duration && report && item.enabled && !item.submenu
            && [lower isEqualToString:durationTitle]) {
            [menu performActionForItemAtIndex:i];
            fprintf(stderr, "metal_hud_report: selected_duration_seconds=%s title=%s\n", duration, item.title.UTF8String);
            started = YES;
        }
        if (item.submenu && depth < 8) started = inspect_hud_menu(item.submenu, report, depth + 1) || started;
    }
    return started;
}
static void capture_start(uint64_t acquisition, id<MTLDevice> device) {
    if (!gCapturePath || acquisition != gCaptureStart) return;
    @try {
        MTLCaptureDescriptor *descriptor = [MTLCaptureDescriptor new];
        descriptor.captureObject = device ?: gCaptureDevice;
        descriptor.destination = MTLCaptureDestinationGPUTraceDocument;
        descriptor.outputURL = [NSURL fileURLWithPath:gCapturePath];
        NSError *error = nil;
        BOOL result = [[MTLCaptureManager sharedCaptureManager] startCaptureWithDescriptor:descriptor error:&error];
        atomic_store(&gCaptureTarget, acquisition + (gCaptureCount - 1));
        atomic_store(&gCaptureBeginNS, mach_absolute_time());
        atomic_store(&gCaptureActive, result);
        fprintf(stderr, "metal_trace_capture: start=%llu target=%llu success=%d error=%s\n",
            (unsigned long long)acquisition, (unsigned long long)atomic_load(&gCaptureTarget), result,
            error ? error.localizedDescription.UTF8String : "");
    } @catch (NSException *exception) {
        fprintf(stderr, "metal_trace_capture: exception=%s\n", exception.reason.UTF8String);
    }
}

@interface TraceBufferState : NSObject {
@public
    Event identity;
    atomic_bool committed;
    uint64_t present_acquisition;
}
@end
@implementation TraceBufferState
@end
@interface TraceDrawableState : NSObject {
@public
    Event identity;
}
@end
@implementation TraceDrawableState
@end
@interface TraceEncoderState : NSObject {
@public
    Event identity;
    atomic_bool ended;
}
@end
@implementation TraceEncoderState
@end

static uint64_t pointer(id value) { return (uint64_t)(uintptr_t)(__bridge void *)value; }
static uint64_t now_ns(void) {
    return (uint64_t)(((long double)mach_absolute_time() * gTimebase.numer) / gTimebase.denom);
}
static uint64_t thread_id(void) { uint64_t value = 0; pthread_threadid_np(NULL, &value); return value; }
static uint64_t thread_cpu_ns(void) {
    struct timespec value;
    if (clock_gettime(CLOCK_THREAD_CPUTIME_ID, &value) != 0) {
        atomic_fetch_add(&gThreadCPUClockFailures, 1);
        return 0;
    }
    return (uint64_t)value.tv_sec * 1000000000 + value.tv_nsec;
}
static void copy_text(char *out, size_t size, const char *text) {
    if (!text) text = "";
    snprintf(out, size, "%s", text);
}
static Event event(const char *kind) {
    Event value = {0};
    copy_text(value.kind, sizeof(value.kind), kind);
    value.ns = now_ns(); value.thread = thread_id(); value.thread_cpu_ns = thread_cpu_ns();
    return value;
}
static BOOL tracing(void) {
    return atomic_load(&gEnabled) && atomic_load(&gNextAcquisition) >= gStartAcquisition;
}
static void emit(Event value) {
    if (!atomic_load(&gEnabled)) return;
    if (atomic_fetch_add(&gRows, 1) >= gMaxRows) {
        atomic_fetch_add(&gDropped, 1); atomic_store(&gEnabled, false); return;
    }
    pthread_mutex_lock(&gRingMutex);
    size_t next = (gHead + 1) % RING_SIZE;
    if (next == gTail) atomic_fetch_add(&gDropped, 1);
    else { gRing[gHead] = value; gHead = next; }
    pthread_mutex_unlock(&gRingMutex);
}
static BOOL parse_positive_u64(const char *text, uint64_t *result) {
    if (!text || !*text) return NO;
    for (const char *p = text; *p; p++) if (*p < '0' || *p > '9') return NO;
    errno = 0;
    char *end = NULL;
    unsigned long long value = strtoull(text, &end, 10);
    if (errno == ERANGE || !end || *end || value == 0 || value > UINT64_MAX) return NO;
    *result = (uint64_t)value;
    return YES;
}
static void capture_configuration_error(const char *parameter, const char *reason) {
    atomic_fetch_add(&gCaptureConfigErrors, 1);
    fprintf(stderr, "metal_trace_capture: config_error parameter=%s reason=%s capture_disabled=1\n",
        parameter, reason);
    Event e = event("capture_config_error");
    copy_text(e.selector, sizeof(e.selector), parameter);
    copy_text(e.name, sizeof(e.name), reason);
    emit(e);
}
static void configure_capture(void) {
    const char *path = getenv("MUDCRAB_METAL_GPU_TRACE_FILE");
    const char *start = getenv("MUDCRAB_METAL_GPU_TRACE_START_ACQUISITION");
    const char *count = getenv("MUDCRAB_METAL_GPU_TRACE_ACQUISITIONS");
    BOOL valid = YES;
    if (start && !parse_positive_u64(start, &gCaptureStart)) {
        capture_configuration_error("MUDCRAB_METAL_GPU_TRACE_START_ACQUISITION", "must be a positive decimal uint64");
        valid = NO;
    }
    if (count && !parse_positive_u64(count, &gCaptureCount)) {
        capture_configuration_error("MUDCRAB_METAL_GPU_TRACE_ACQUISITIONS", "must be a positive decimal uint64");
        valid = NO;
    }
    if (valid && gCaptureCount - 1 > UINT64_MAX - gCaptureStart) {
        capture_configuration_error("capture_target", "start + count - 1 overflows uint64");
        valid = NO;
    }
    if (valid && path && *path) gCapturePath = [NSString stringWithUTF8String:path];
}
static void write_string(FILE *out, const char *text) {
    fputc('"', out);
    for (const unsigned char *p = (const unsigned char *)text; *p; p++) {
        if (*p == '"' || *p == '\\') { fputc('\\', out); fputc(*p, out); }
        else if (*p < 32) fprintf(out, "\\u%04x", *p);
        else fputc(*p, out);
    }
    fputc('"', out);
}
static void write_event(Event *e) {
    fputs("{\"type\":", gFile); write_string(gFile, e->kind);
    fputs(",\"name\":", gFile); write_string(gFile, e->name);
    fputs(",\"selector\":", gFile); write_string(gFile, e->selector);
    fprintf(gFile, ",\"host_ns\":%llu,\"end_ns\":%llu,\"thread\":%llu,\"thread_cpu_ns\":%llu,"
        "\"object\":%llu,\"queue\":%llu,\"device_registry_id\":%llu,\"layer\":%llu,"
        "\"cb_id\":%llu,\"acquisition_id\":%llu,\"drawable_id\":%llu,\"texture\":%llu,"
        "\"created_ns\":%llu,\"count\":%llu,\"extra\":%llu,\"gpu_start_s\":%.9f,"
        "\"gpu_end_s\":%.9f,\"kernel_start_s\":%.9f,\"kernel_end_s\":%.9f,"
        "\"presented_s\":%.9f,\"parameter_s\":%.9f,\"status\":%u,\"width\":%u,\"height\":%u,"
        "\"gpu_time_valid\":%s}\n",
        (unsigned long long)e->ns, (unsigned long long)e->end_ns, (unsigned long long)e->thread,
        (unsigned long long)e->thread_cpu_ns, (unsigned long long)e->object, (unsigned long long)e->queue,
        (unsigned long long)e->device, (unsigned long long)e->layer, (unsigned long long)e->cb,
        (unsigned long long)e->acquisition, (unsigned long long)e->drawable, (unsigned long long)e->texture,
        (unsigned long long)e->created_ns, (unsigned long long)e->count, (unsigned long long)e->extra,
        e->gpu_start, e->gpu_end, e->kernel_start, e->kernel_end, e->presented, e->parameter,
        e->status, e->width, e->height,
        e->status == MTLCommandBufferStatusCompleted && e->gpu_start > 0 && e->gpu_end >= e->gpu_start
            ? "true" : "false");
}
static void capture_stop_if_ready(void) {
    if (!atomic_load(&gCaptureActive)) return;
    uint64_t begin = atomic_load(&gCaptureBeginNS);
    double elapsed = (double)(mach_absolute_time() - begin) * gTimebase.numer / gTimebase.denom / 1e9;
    BOOL completed = atomic_load(&gCaptureStopRequested);
    if (!completed && elapsed < 10.0) return;
    [[MTLCaptureManager sharedCaptureManager] stopCapture];
    atomic_store(&gCaptureActive, false);
    Event e = event("capture_stop"); e.acquisition = atomic_load(&gCaptureTarget); e.status = completed ? 1 : 0;
    copy_text(e.name, sizeof(e.name), completed ? "target_present_buffer_completed" : "timeout_incomplete");
    write_event(&e);
}
static void write_health(void) {
    fprintf(gFile, "{\"type\":\"health\",\"host_ns\":%llu,\"rows_attempted\":%llu,"
        "\"dropped\":%llu,\"exceptions\":%llu,\"hook_failures\":%llu,\"factory_calls\":%llu,"
        "\"commits\":%llu,\"completed\":%llu,\"scheduled\":%llu,\"present_encode_calls\":%llu,"
        "\"presented_callbacks\":%llu,\"acquired\":%llu,\"nil_acquires\":%llu,"
        "\"matched_render_attachments\":%llu,\"unmatched_render_attachments\":%llu,"
        "\"duplicate_commit_wrappers\":%llu,\"thread_cpu_clock_failures\":%llu,"
        "\"capture_config_errors\":%llu,\"enabled\":%s}\n",
        (unsigned long long)now_ns(), (unsigned long long)atomic_load(&gRows),
        (unsigned long long)atomic_load(&gDropped), (unsigned long long)atomic_load(&gExceptions),
        (unsigned long long)atomic_load(&gHookFailures), (unsigned long long)atomic_load(&gFactoryCalls),
        (unsigned long long)atomic_load(&gCommits), (unsigned long long)atomic_load(&gCompleted),
        (unsigned long long)atomic_load(&gScheduled), (unsigned long long)atomic_load(&gPresents),
        (unsigned long long)atomic_load(&gPresented), (unsigned long long)atomic_load(&gAcquired),
        (unsigned long long)atomic_load(&gEmptyAcquire), (unsigned long long)atomic_load(&gTexturesMatched),
        (unsigned long long)atomic_load(&gTextureMisses), (unsigned long long)atomic_load(&gDuplicateCommit),
        (unsigned long long)atomic_load(&gThreadCPUClockFailures),
        (unsigned long long)atomic_load(&gCaptureConfigErrors),
        atomic_load(&gEnabled) ? "true" : "false");
}
static void *writer(void *unused) {
    (void)unused; Event batch[128]; uint64_t lastHealth = now_ns();
    for (;;) {
        size_t count = 0;
        pthread_mutex_lock(&gRingMutex);
        while (gTail != gHead && count < 128) { batch[count++] = gRing[gTail]; gTail = (gTail + 1) % RING_SIZE; }
        BOOL empty = gHead == gTail;
        pthread_mutex_unlock(&gRingMutex);
        for (size_t i = 0; i < count; i++) write_event(&batch[i]);
        @autoreleasepool { capture_stop_if_ready(); }
        if (now_ns() - lastHealth > 1000000000) { write_health(); fflush(gFile); lastHealth = now_ns(); }
        if (atomic_load(&gStop) && empty) break;
        if (count == 0) { struct timespec pause = {0, 5000000}; nanosleep(&pause, NULL); }
    }
    write_health(); fflush(gFile); return NULL;
}

// Install on the discovered concrete class, adding an override if the method was
// inherited. Each block captures its own IMP, so a subclass's super call remains
// valid. An inherited already-installed wrapper is left alone.
static void install(Class cls, SEL sel, id (^makeBlock)(IMP)) {
    if (!cls) return;
    pthread_mutex_lock(&gHookMutex);
    Method method = class_getInstanceMethod(cls, sel);
    IMP original = method ? method_getImplementation(method) : NULL;
    for (size_t i = 0; i < gHookCount; i++) {
        if ((gHooks[i].cls == cls && gHooks[i].sel == sel) || gHooks[i].wrapper == original) {
            pthread_mutex_unlock(&gHookMutex); return;
        }
    }
    if (!original || gHookCount == HOOK_LIMIT) {
        atomic_fetch_add(&gHookFailures, 1); pthread_mutex_unlock(&gHookMutex); return;
    }
    id block = makeBlock(original);
    IMP wrapper = imp_implementationWithBlock(block);
    if (!class_addMethod(cls, sel, wrapper, method_getTypeEncoding(method)))
        class_replaceMethod(cls, sel, wrapper, method_getTypeEncoding(method));
    gHooks[gHookCount++] = (Hook){cls, sel, wrapper};
    pthread_mutex_unlock(&gHookMutex);
    Event e = event("hook_installed"); copy_text(e.name, sizeof(e.name), class_getName(cls));
    copy_text(e.selector, sizeof(e.selector), sel_getName(sel)); emit(e);
}
static void hook_buffer(id<MTLCommandBuffer> buffer);
static void hook_queue(id<MTLCommandQueue> queue);
static void hook_drawable(id<CAMetalDrawable> drawable);

static TraceBufferState *buffer_state(id<MTLCommandBuffer> buffer, id<MTLCommandQueue> queue) {
    TraceBufferState *state = objc_getAssociatedObject(buffer, &kBufferState);
    if (!state && tracing()) {
        state = [TraceBufferState new]; state->identity = event("cb_create");
        state->identity.cb = atomic_fetch_add(&gNextCB, 1) + 1;
        state->identity.object = pointer(buffer); state->identity.queue = pointer(queue ?: buffer.commandQueue);
        state->identity.device = buffer.device.registryID; state->identity.created_ns = state->identity.ns;
        copy_text(state->identity.name, sizeof(state->identity.name), object_getClassName(buffer));
        objc_setAssociatedObject(buffer, &kBufferState, state, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        emit(state->identity);
    }
    return state;
}
static Event buffer_event(id<MTLCommandBuffer> buffer, TraceBufferState *state, const char *kind) {
    Event e = state->identity; copy_text(e.kind, sizeof(e.kind), kind);
    e.ns = now_ns(); e.thread = thread_id(); e.thread_cpu_ns = thread_cpu_ns();
    copy_text(e.name, sizeof(e.name), buffer.label.UTF8String);
    return e;
}
static void adopt_buffer(id<MTLCommandBuffer> buffer, id<MTLCommandQueue> queue) {
    if (!buffer || !atomic_load(&gEnabled)) return;
    @try { hook_buffer(buffer); (void)buffer_state(buffer, queue); }
    @catch (NSException *exception) { (void)exception; atomic_fetch_add(&gExceptions, 1); }
}
static void remember_texture(Event e) {
    if (pthread_mutex_trylock(&gTextureMutex) != 0) { atomic_fetch_add(&gDropped, 1); return; }
    size_t slot = (e.texture >> 4) % TEXTURE_SLOTS;
    for (size_t n = 0; n < TEXTURE_SLOTS; n++) {
        size_t i = (slot + n) % TEXTURE_SLOTS;
        if (!gTextures[i].texture || gTextures[i].texture == e.texture) {
            gTextures[i] = (TextureIdentity){e.texture, e.acquisition, e.drawable, e.layer};
            pthread_mutex_unlock(&gTextureMutex); return;
        }
    }
    atomic_fetch_add(&gDropped, 1); pthread_mutex_unlock(&gTextureMutex);
}
static BOOL match_texture(Event *e, id<MTLTexture> texture) {
    if (!texture) return NO;
    uint64_t key = pointer(texture);
    if (pthread_mutex_trylock(&gTextureMutex) != 0) { atomic_fetch_add(&gDropped, 1); return NO; }
    size_t slot = (key >> 4) % TEXTURE_SLOTS; BOOL matched = NO;
    for (size_t n = 0; n < TEXTURE_SLOTS; n++) {
        TextureIdentity saved = gTextures[(slot + n) % TEXTURE_SLOTS];
        if (!saved.texture) continue;
        if (saved.texture == key) {
            e->texture = key; e->acquisition = saved.acquisition; e->drawable = saved.drawable; e->layer = saved.layer;
            matched = YES; break;
        }
    }
    pthread_mutex_unlock(&gTextureMutex);
    if (matched) atomic_fetch_add(&gTexturesMatched, 1); else atomic_fetch_add(&gTextureMisses, 1);
    return matched;
}
static void forget_texture(Event e) {
    pthread_mutex_lock(&gTextureMutex);
    for (size_t i = 0; i < TEXTURE_SLOTS; i++) {
        if (gTextures[i].texture == e.texture && gTextures[i].acquisition == e.acquisition)
            gTextures[i] = (TextureIdentity){0};
    }
    pthread_mutex_unlock(&gTextureMutex);
}
static void adopt_encoder(id<MTLCommandEncoder> encoder, Event e) {
    if (!encoder || !tracing() || objc_getAssociatedObject(encoder, &kEncoderState)) return;
    TraceEncoderState *state = [TraceEncoderState new];
    e.object = pointer(encoder); copy_text(e.kind, sizeof(e.kind), "encoder_begin"); state->identity = e;
    objc_setAssociatedObject(encoder, &kEncoderState, state, OBJC_ASSOCIATION_RETAIN_NONATOMIC); emit(e);
    SEL sel = @selector(endEncoding);
    install(object_getClass(encoder), sel, ^id(IMP original) {
        return ^(id object) {
            TraceEncoderState *saved = objc_getAssociatedObject(object, &kEncoderState);
            BOOL first = saved && !atomic_exchange(&saved->ended, true);
            ((void (*)(id, SEL))original)(object, sel);
            if (first && tracing()) {
                Event end = saved->identity; copy_text(end.kind, sizeof(end.kind), "encoder_end");
                end.end_ns = now_ns(); end.extra = end.thread_cpu_ns;
                end.count = end.thread; end.thread = thread_id(); end.thread_cpu_ns = thread_cpu_ns();
                copy_text(end.name, sizeof(end.name), ((id<MTLCommandEncoder>)object).label.UTF8String); emit(end);
            }
        };
    });
}
static void hook_encoder_factory(Class cls, SEL sel, BOOL descriptorArgument, BOOL renderPass) {
    install(cls, sel, ^id(IMP original) {
        if (descriptorArgument) return ^id(id object, id descriptor) {
            TraceBufferState *saved = buffer_state(object, nil);
            Event e = saved ? buffer_event(object, saved, "encoder_begin") : (Event){0};
            copy_text(e.selector, sizeof(e.selector), sel_getName(sel));
            if (saved && renderPass) {
                MTLRenderPassDescriptor *pass = descriptor;
                id<MTLTexture> target = pass.colorAttachments[0].texture ?: pass.depthAttachment.texture;
                e.width = (uint32_t)target.width; e.height = (uint32_t)target.height;
                e.parameter = (double)target.pixelFormat;
                for (NSUInteger i = 0; i < 8; i++) {
                    if (match_texture(&e, pass.colorAttachments[i].texture)
                        || match_texture(&e, pass.colorAttachments[i].resolveTexture)) break;
                }
            }
            id result = ((id (*)(id, SEL, id))original)(object, sel, descriptor);
            if (saved) adopt_encoder(result, e); return result;
        };
        return ^id(id object) {
            TraceBufferState *saved = buffer_state(object, nil);
            Event e = saved ? buffer_event(object, saved, "encoder_begin") : (Event){0};
            copy_text(e.selector, sizeof(e.selector), sel_getName(sel));
            id result = ((id (*)(id, SEL))original)(object, sel);
            if (saved) adopt_encoder(result, e); return result;
        };
    });
}
static void hook_buffer(id<MTLCommandBuffer> buffer) {
    Class cls = object_getClass(buffer); SEL commit = @selector(commit);
    install(cls, commit, ^id(IMP original) {
        return ^(id<MTLCommandBuffer> object) {
            TraceBufferState *saved = tracing() ? buffer_state(object, nil) : nil;
            BOOL first = saved && !atomic_exchange(&saved->committed, true);
            if (saved && !first) atomic_fetch_add(&gDuplicateCommit, 1);
            Event begin = {0};
            if (first) {
                @try {
                    begin = buffer_event(object, saved, "commit_begin");
                    begin.acquisition = saved->present_acquisition; emit(begin); atomic_fetch_add(&gCommits, 1);
                    // Blocks capture numeric Event values only. No object is retained.
                    [object addScheduledHandler:^(id<MTLCommandBuffer> callback) {
                        @try {
                            Event e = begin; copy_text(e.kind, sizeof(e.kind), "scheduled");
                            e.ns = now_ns(); e.thread = thread_id(); e.thread_cpu_ns = thread_cpu_ns();
                            e.kernel_start = callback.kernelStartTime; e.kernel_end = callback.kernelEndTime;
                            e.status = (uint32_t)callback.status; emit(e); atomic_fetch_add(&gScheduled, 1);
                        } @catch (NSException *exception) { (void)exception; atomic_fetch_add(&gExceptions, 1); }
                    }];
                    [object addCompletedHandler:^(id<MTLCommandBuffer> callback) {
                        @try {
                            Event e = begin; copy_text(e.kind, sizeof(e.kind), "completed");
                            e.ns = now_ns(); e.thread = thread_id(); e.thread_cpu_ns = thread_cpu_ns();
                            e.kernel_start = callback.kernelStartTime; e.kernel_end = callback.kernelEndTime;
                            e.gpu_start = callback.GPUStartTime; e.gpu_end = callback.GPUEndTime;
                            e.status = (uint32_t)callback.status; e.extra = (uint64_t)callback.error.code;
                            emit(e); atomic_fetch_add(&gCompleted, 1);
                            if (atomic_load(&gCaptureActive) && e.acquisition == atomic_load(&gCaptureTarget))
                                atomic_store(&gCaptureStopRequested, true);
                        } @catch (NSException *exception) { (void)exception; atomic_fetch_add(&gExceptions, 1); }
                    }];
                } @catch (NSException *exception) { (void)exception; atomic_fetch_add(&gExceptions, 1); }
            }
            ((void (*)(id, SEL))original)(object, commit);
            if (first) { Event e = begin; copy_text(e.kind, sizeof(e.kind), "commit_end");
                e.end_ns = now_ns(); e.thread_cpu_ns = thread_cpu_ns(); emit(e); }
        };
    });
    for (NSString *name in @[@"presentDrawable:", @"presentDrawable:atTime:", @"presentDrawable:afterMinimumDuration:"]) {
        SEL sel = NSSelectorFromString(name); BOOL timed = [name componentsSeparatedByString:@":"].count == 3;
        install(cls, sel, ^id(IMP original) {
            if (timed) return ^(id object, id drawable, double parameter) {
                TraceBufferState *saved = tracing() ? buffer_state(object, nil) : nil;
                TraceDrawableState *draw = objc_getAssociatedObject(drawable, &kDrawableState);
                Event e = saved ? buffer_event(object, saved, "present_encode_begin") : (Event){0};
                if (draw) { e.acquisition = draw->identity.acquisition; e.drawable = draw->identity.drawable;
                    e.layer = draw->identity.layer; e.texture = draw->identity.texture;
                    if (saved) saved->present_acquisition = e.acquisition; }
                e.parameter = parameter; copy_text(e.selector, sizeof(e.selector), sel_getName(sel));
                if (saved) { emit(e); atomic_fetch_add(&gPresents, 1); }
                ((void (*)(id, SEL, id, double))original)(object, sel, drawable, parameter);
                if (saved) { copy_text(e.kind, sizeof(e.kind), "present_encode_end"); e.end_ns = now_ns(); emit(e); }
            };
            return ^(id object, id drawable) {
                TraceBufferState *saved = tracing() ? buffer_state(object, nil) : nil;
                TraceDrawableState *draw = objc_getAssociatedObject(drawable, &kDrawableState);
                Event e = saved ? buffer_event(object, saved, "present_encode_begin") : (Event){0};
                if (draw) { e.acquisition = draw->identity.acquisition; e.drawable = draw->identity.drawable;
                    e.layer = draw->identity.layer; e.texture = draw->identity.texture;
                    if (saved) saved->present_acquisition = e.acquisition; }
                copy_text(e.selector, sizeof(e.selector), sel_getName(sel));
                if (saved) { emit(e); atomic_fetch_add(&gPresents, 1); }
                ((void (*)(id, SEL, id))original)(object, sel, drawable);
                if (saved) { copy_text(e.kind, sizeof(e.kind), "present_encode_end"); e.end_ns = now_ns(); emit(e); }
            };
        });
    }
    hook_encoder_factory(cls, @selector(renderCommandEncoderWithDescriptor:), YES, YES);
    hook_encoder_factory(cls, @selector(computeCommandEncoder), NO, NO);
    hook_encoder_factory(cls, @selector(computeCommandEncoderWithDescriptor:), YES, NO);
    hook_encoder_factory(cls, @selector(blitCommandEncoder), NO, NO);
    hook_encoder_factory(cls, @selector(blitCommandEncoderWithDescriptor:), YES, NO);
}
static void hook_queue(id<MTLCommandQueue> queue) {
    Class cls = object_getClass(queue);
    for (NSString *name in @[@"commandBuffer", @"commandBufferWithUnretainedReferences", @"commandBufferWithDescriptor:"]) {
        SEL sel = NSSelectorFromString(name); BOOL argument = [name hasSuffix:@":"];
        install(cls, sel, ^id(IMP original) {
            if (argument) return ^id(id object, id descriptor) {
                id result = ((id (*)(id, SEL, id))original)(object, sel, descriptor);
                atomic_fetch_add(&gFactoryCalls, 1); adopt_buffer(result, object); return result;
            };
            return ^id(id object) {
                id result = ((id (*)(id, SEL))original)(object, sel);
                atomic_fetch_add(&gFactoryCalls, 1); adopt_buffer(result, object); return result;
            };
        });
    }
}
static void hook_device(id<MTLDevice> device) {
    Class cls = object_getClass(device);
    SEL counterSel = @selector(newCounterSampleBufferWithDescriptor:error:);
    if ([device respondsToSelector:counterSel]) install(cls, counterSel, ^id(IMP original) {
        return ^id __attribute__((ns_returns_retained)) (id object, id descriptor, NSError *__autoreleasing *error) {
            typedef id __attribute__((ns_returns_retained)) (*NewCounter)(id, SEL, id, NSError *__autoreleasing *);
            id result = ((NewCounter)original)(object, counterSel, descriptor, error);
            Event e = event("counter_sample_buffer_create"); e.object = pointer(result);
            e.device = ((id<MTLDevice>)object).registryID; e.status = result ? 1 : 0;
            copy_text(e.name, sizeof(e.name), ((MTLCounterSampleBufferDescriptor *)descriptor).label.UTF8String);
            emit(e); return result;
        };
    });
    for (NSString *name in @[@"newCommandQueue", @"newCommandQueueWithMaxCommandBufferCount:"]) {
        SEL sel = NSSelectorFromString(name); BOOL argument = [name hasSuffix:@":"];
        install(cls, sel, ^id(IMP original) {
            void (^adopt)(id) = ^(id result) {
                if (!result || !atomic_load(&gEnabled)) return;
                hook_queue(result); Event e = event("queue_discovered"); e.object = pointer(result); e.queue = e.object;
                e.device = ((id<MTLCommandQueue>)result).device.registryID;
                copy_text(e.name, sizeof(e.name), object_getClassName(result)); emit(e);
            };
            if (argument) return ^id __attribute__((ns_returns_retained)) (id object, NSUInteger maximum) {
                typedef id __attribute__((ns_returns_retained)) (*NewQueueMaximum)(id, SEL, NSUInteger);
                id result = ((NewQueueMaximum)original)(object, sel, maximum); adopt(result); return result;
            };
            return ^id __attribute__((ns_returns_retained)) (id object) {
                typedef id __attribute__((ns_returns_retained)) (*NewQueue)(id, SEL);
                id result = ((NewQueue)original)(object, sel); adopt(result); return result;
            };
        });
    }
}
static void hook_drawable(id<CAMetalDrawable> drawable) {
    Class cls = object_getClass(drawable);
    for (NSString *name in @[@"present", @"presentAtTime:", @"presentAfterMinimumDuration:"]) {
        SEL sel = NSSelectorFromString(name); BOOL timed = [name hasSuffix:@":"];
        install(cls, sel, ^id(IMP original) {
            if (timed) return ^(id object, double parameter) {
                TraceDrawableState *saved = objc_getAssociatedObject(object, &kDrawableState);
                Event e = saved ? saved->identity : (Event){0}; e.ns = now_ns(); e.thread = thread_id();
                e.parameter = parameter; copy_text(e.kind, sizeof(e.kind), "drawable_present_begin");
                copy_text(e.selector, sizeof(e.selector), sel_getName(sel)); if (saved && tracing()) emit(e);
                ((void (*)(id, SEL, double))original)(object, sel, parameter);
                if (saved && tracing()) { copy_text(e.kind, sizeof(e.kind), "drawable_present_end"); e.end_ns = now_ns(); emit(e); }
            };
            return ^(id object) {
                TraceDrawableState *saved = objc_getAssociatedObject(object, &kDrawableState);
                Event e = saved ? saved->identity : (Event){0}; e.ns = now_ns(); e.thread = thread_id();
                copy_text(e.kind, sizeof(e.kind), "drawable_present_begin");
                copy_text(e.selector, sizeof(e.selector), sel_getName(sel)); if (saved && tracing()) emit(e);
                ((void (*)(id, SEL))original)(object, sel);
                if (saved && tracing()) { copy_text(e.kind, sizeof(e.kind), "drawable_present_end"); e.end_ns = now_ns(); emit(e); }
            };
        });
    }
}
static void hook_layer(void) {
    if (getenv("MUDCRAB_METAL_DIAGNOSTIC_DISPLAY_SYNC")) {
        SEL syncSel = @selector(setDisplaySyncEnabled:);
        install([CAMetalLayer class], syncSel, ^id(IMP original) {
            return ^(id object, BOOL requested) {
                BOOL applied = strcmp(getenv("MUDCRAB_METAL_DIAGNOSTIC_DISPLAY_SYNC"), "1") == 0;
                ((void (*)(id, SEL, BOOL))original)(object, syncSel, applied);
                Event e = event("display_sync_override"); e.layer = pointer(object);
                e.count = requested; e.extra = applied; emit(e);
            };
        });
    }
    SEL sel = @selector(nextDrawable);
    install([CAMetalLayer class], sel, ^id(IMP original) {
        return ^id(CAMetalLayer *layer) {
            uint64_t acquisition = atomic_fetch_add(&gNextAcquisition, 1) + 1;
            capture_start(acquisition, layer.device);
            if (acquisition >= 1220 && getenv("MUDCRAB_METAL_HUD_MENU_PROBE")
                && !atomic_exchange(&gHUDMenuQueued, true)) {
                dispatch_async(dispatch_get_main_queue(), ^{
                    BOOL started = inspect_hud_menu(NSApplication.sharedApplication.mainMenu, NO, 0);
                    fprintf(stderr, "metal_hud_report: menu_inspected=1 started=%d\n", started);
                });
            }
            BOOL active = tracing(); Event e = {0};
            if (active) { e = event("acquire_begin"); e.layer = pointer(layer); e.acquisition = acquisition; emit(e); }
            id<CAMetalDrawable> drawable = ((id (*)(id, SEL))original)(layer, sel);
            if (!active) return drawable;
            @try {
                e.end_ns = now_ns(); copy_text(e.kind, sizeof(e.kind), "acquire_end");
                e.thread_cpu_ns = thread_cpu_ns(); e.object = pointer(drawable);
                e.width = (uint32_t)layer.drawableSize.width; e.height = (uint32_t)layer.drawableSize.height;
                e.count = layer.maximumDrawableCount;
                e.extra = (layer.displaySyncEnabled ? 1u : 0u) | (layer.presentsWithTransaction ? 2u : 0u)
                    | (layer.allowsNextDrawableTimeout ? 4u : 0u);
                e.device = layer.device.registryID;
                if (drawable) {
                    atomic_fetch_add(&gAcquired, 1); e.drawable = drawable.drawableID; e.texture = pointer(drawable.texture);
                    copy_text(e.name, sizeof(e.name), object_getClassName(drawable));
                    if (!gMinimal) {
                    TraceDrawableState *saved = [TraceDrawableState new]; saved->identity = e;
                    objc_setAssociatedObject(drawable, &kDrawableState, saved, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
                    remember_texture(e); hook_drawable(drawable);
                    // Numeric snapshot only: retaining the drawable here would perturb the pool.
                    [drawable addPresentedHandler:^(id<MTLDrawable> callback) {
                        @try {
                            Event presented = e; copy_text(presented.kind, sizeof(presented.kind), "presented");
                            presented.ns = now_ns(); presented.thread = thread_id(); presented.thread_cpu_ns = thread_cpu_ns();
                            presented.presented = callback.presentedTime; presented.status = presented.presented > 0 ? 1 : 0;
                            emit(presented); atomic_fetch_add(&gPresented, 1); forget_texture(e);
                        } @catch (NSException *exception) { (void)exception; atomic_fetch_add(&gExceptions, 1); }
                    }];
                    }
                } else atomic_fetch_add(&gEmptyAcquire, 1);
                emit(e);
            } @catch (NSException *exception) { (void)exception; atomic_fetch_add(&gExceptions, 1); }
            return drawable;
        };
    });
}

__attribute__((constructor)) static void start(void) {
    const char *path = getenv("MUDCRAB_METAL_TRACE_FILE"); if (!path || !*path) return;
    gMinimal = getenv("MUDCRAB_METAL_TRACE_ACQUIRE_ONLY") != NULL;
    mach_timebase_info(&gTimebase); if (!gTimebase.denom) { gTimebase.numer = 1; gTimebase.denom = 1; }
    const char *limit = getenv("MUDCRAB_METAL_TRACE_MAX_ROWS"); if (limit) gMaxRows = strtoull(limit, NULL, 10);
    const char *warmup = getenv("MUDCRAB_METAL_TRACE_START_ACQUISITION");
    if (warmup) gStartAcquisition = strtoull(warmup, NULL, 10);
    gFile = fopen(path, "wx"); if (!gFile) { perror("MUDCRAB_METAL_TRACE_FILE"); return; }
    struct timespec wall; clock_gettime(CLOCK_REALTIME, &wall);
    fprintf(gFile, "{\"type\":\"trace_begin\",\"schema\":1,\"pid\":%d,\"wall_unix_s\":%lld,\"wall_ns\":%ld,"
        "\"mach_ticks\":%llu,\"host_ns\":%llu,\"ca_current_media_s\":%.9f,\"timebase_numer\":%u,"
        "\"timebase_denom\":%u,\"max_rows\":%llu,\"start_acquisition\":%llu,"
        "\"acquisition_is_engine_frame_id\":false,\"observation_only\":%s}\n",
        getpid(), (long long)wall.tv_sec, wall.tv_nsec, (unsigned long long)mach_absolute_time(),
        (unsigned long long)now_ns(), CACurrentMediaTime(), gTimebase.numer, gTimebase.denom,
        (unsigned long long)gMaxRows, (unsigned long long)gStartAcquisition,
        getenv("MUDCRAB_METAL_DIAGNOSTIC_DISPLAY_SYNC") ? "false" : "true");
    atomic_store(&gEnabled, true);
    if (getenv("MUDCRAB_METAL_PROFILE_ACTIVATE_WINDOW")) {
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 4 * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
            NSApplication *app = NSApplication.sharedApplication;
            for (NSWindow *window in app.windows) {
                fprintf(stderr, "metal_profile_window: title=%s visible=%d occlusion=%lu activate_requested=1\n",
                    window.title.UTF8String, window.visible, (unsigned long)window.occlusionState);
                if ([window.title containsString:@"Mudcrab"]) {
                    [window makeKeyAndOrderFront:nil];
                    [window orderFrontRegardless];
                }
            }
            [app activate];
        });
    }
    if (pthread_create(&gWriter, NULL, writer, NULL) != 0) { atomic_store(&gEnabled, false); fclose(gFile); gFile = NULL; return; }
    @autoreleasepool {
        @try {
            id<MTLDevice> device = MTLCreateSystemDefaultDevice();
            gCaptureDevice = device;
            configure_capture();
            NSArray<id<MTLDevice>> *devices = MTLCopyAllDevices();
            if (!gMinimal) {
                for (id<MTLDevice> found in devices) hook_device(found);
                hook_device(device);
            }
            hook_layer();
            id<MTLCommandQueue> queue = [device newCommandQueue];
            id<MTLCommandBuffer> buffer = [queue commandBuffer];
            Event probe = event("probe"); probe.object = pointer(buffer); probe.queue = pointer(queue);
            probe.device = device.registryID; copy_text(probe.name, sizeof(probe.name), object_getClassName(buffer));
            MTLCaptureManager *manager = [MTLCaptureManager sharedCaptureManager];
            probe.count = [manager supportsDestination:MTLCaptureDestinationGPUTraceDocument];
            probe.extra = [manager supportsDestination:MTLCaptureDestinationDeveloperTools]; emit(probe);
            printf("metal_trace: pid=%d device=%s queue=%s command_buffer=%s GPUTraceDocument=%llu DeveloperTools=%llu path=%s\n",
                getpid(), object_getClassName(device), object_getClassName(queue), object_getClassName(buffer),
                (unsigned long long)probe.count, (unsigned long long)probe.extra, path);
            // Probe buffer is never committed and no GPU capture is started.
        } @catch (NSException *exception) { (void)exception; atomic_fetch_add(&gExceptions, 1); }
    }
}
__attribute__((destructor)) static void stop(void) {
    if (!gFile) return;
    atomic_store(&gEnabled, false); atomic_store(&gStop, true); pthread_join(gWriter, NULL);
    if (atomic_load(&gCaptureActive)) [[MTLCaptureManager sharedCaptureManager] stopCapture];
    fclose(gFile); gFile = NULL;
}
