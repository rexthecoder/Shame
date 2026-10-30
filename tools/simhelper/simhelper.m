// shame-sim: headless touch / button injection for the iOS Simulator.
//
// Replaces the external `idb` CLI. Uses the same private-framework route as
// facebook/idb (MIT, https://github.com/facebook/idb): CoreSimulator finds the
// SimDevice, SimulatorKit builds Indigo HID messages and SimDeviceLegacyHIDClient
// delivers them. The Indigo layout below is adapted from idb's Indigo.h.
//
// Usage:
//   shame-sim tap   <udid> <nx> <ny>
//   shame-sim swipe <udid> <nx1> <ny1> <nx2> <ny2> [duration_ms]
//   shame-sim home  <udid>
//   shame-sim serve <udid> [--stream]   # long-running: reads commands from stdin, one per line:
//                            #   down|move|up <nx> <ny>  /  tap <nx> <ny>  /  home
// Coordinates are normalized (0..1, top-left origin). `serve` exits on stdin EOF.
//
// With --stream, `serve` also writes the simulator's screen to stdout, half-resolution BGRA,
// one frame per change: a 16-byte header ("SFRM", u32 width, u32 height, u32 payload bytes,
// little endian) followed by width*height*4 bytes. It reads the display's IOSurface directly,
// so it is much faster than `simctl io screenshot`.

#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import <dlfcn.h>
#import <mach/mach_time.h>
#import <malloc/malloc.h>
#import <IOSurface/IOSurface.h>
#import <objc/message.h>

#pragma pack(push, 4)
typedef struct {
  unsigned int field1, field2, eventMask;
  double xRatio, yRatio, field6, field7, field8;
  unsigned int range, touch, field11, field12, field13;
  double field14, field15, field16, field17, field18;
} IndigoTouch;
typedef struct { double a, b, c, d; } IndigoQuad;
typedef union {
  IndigoTouch touch;
  IndigoQuad pad[4];
} IndigoEvent;
typedef struct {
  unsigned int eventKind;
  unsigned long long timestamp;
  unsigned int field3;
  IndigoEvent event;
} IndigoPayload;
typedef struct {
  unsigned char header[0x18];
  unsigned int innerSize;
  unsigned char eventType;
  IndigoPayload payload;
} IndigoMessage;
#pragma pack(pop)

static const unsigned kTouchTarget = 0x32;
static const int kButtonTargetHardware = 0x33;

typedef IndigoMessage *(*MouseFn)(CGPoint *, CGPoint *, unsigned, NSUInteger, CGSize, unsigned);
typedef IndigoMessage *(*ButtonFn)(int, int, int);

static MouseFn gMouse;
static ButtonFn gButton;
static id gClient;
static int gFailed;
static id gDevice;
static dispatch_queue_t gQueue;

static void die(NSString *msg) {
  fprintf(stderr, "shame-sim: %s\n", msg.UTF8String);
  exit(1);
}

static NSString *developerDir(void) {
  NSTask *t = [NSTask new];
  t.launchPath = @"/usr/bin/xcode-select";
  t.arguments = @[ @"-p" ];
  NSPipe *p = [NSPipe pipe];
  t.standardOutput = p;
  [t launch];
  NSData *d = [p.fileHandleForReading readDataToEndOfFile];
  [t waitUntilExit];
  NSString *s = [[[NSString alloc] initWithData:d encoding:NSUTF8StringEncoding]
      stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceAndNewlineCharacterSet];
  return s.length ? s : @"/Applications/Xcode.app/Contents/Developer";
}

static void setup(NSString *udid) {
  NSString *dev = developerDir();
  if (!dlopen("/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator", RTLD_NOW))
    die(@"CoreSimulator.framework not found (is Xcode installed?)");
  NSString *kit = [dev stringByAppendingPathComponent:
      @"Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit"];
  void *h = dlopen(kit.UTF8String, RTLD_NOW);
  if (!h) die(@"SimulatorKit.framework not found in the active Xcode");
  gMouse = (MouseFn)dlsym(h, "IndigoHIDMessageForMouseNSEvent");
  gButton = (ButtonFn)dlsym(h, "IndigoHIDMessageForButton");
  if (!gMouse || !gButton) die(@"SimulatorKit is missing the Indigo HID builders");

  Class ctxClass = NSClassFromString(@"SimServiceContext");
  NSError *err = nil;
  id ctx = ((id(*)(Class, SEL, id, NSError **))objc_msgSend)(
      ctxClass, NSSelectorFromString(@"sharedServiceContextForDeveloperDir:error:"), dev, &err);
  if (!ctx) die([NSString stringWithFormat:@"no SimServiceContext: %@", err]);
  id set = ((id(*)(id, SEL, NSError **))objc_msgSend)(
      ctx, NSSelectorFromString(@"defaultDeviceSetWithError:"), &err);
  if (!set) die([NSString stringWithFormat:@"no device set: %@", err]);
  id device = nil;
  for (id d in ((NSArray * (*)(id, SEL)) objc_msgSend)(set, NSSelectorFromString(@"devices"))) {
    NSUUID *u = ((NSUUID * (*)(id, SEL)) objc_msgSend)(d, NSSelectorFromString(@"UDID"));
    if ([u.UUIDString caseInsensitiveCompare:udid] == NSOrderedSame) { device = d; break; }
  }
  if (!device) die([NSString stringWithFormat:@"simulator %@ not found", udid]);
  gDevice = device;

  Class cc = NSClassFromString(@"_TtC12SimulatorKit24SimDeviceLegacyHIDClient")
          ?: NSClassFromString(@"SimulatorKit.SimDeviceLegacyHIDClient");
  if (!cc) die(@"SimDeviceLegacyHIDClient class not found");
  id c = ((id(*)(id, SEL))objc_msgSend)(cc, NSSelectorFromString(@"alloc"));
  gClient = ((id(*)(id, SEL, id, NSError **))objc_msgSend)(
      c, NSSelectorFromString(@"initWithDevice:error:"), device, &err);
  if (!gClient) die([NSString stringWithFormat:@"HID client failed (device booted?): %@", err]);
  gQueue = dispatch_queue_create("shame.sim.hid", DISPATCH_QUEUE_SERIAL);
}

static void sendMessage(void *msg) {
  dispatch_semaphore_t sem = dispatch_semaphore_create(0);
  __block NSError *failure = nil;
  @try {
    ((void (*)(id, SEL, void *, BOOL, dispatch_queue_t, void (^)(NSError *)))objc_msgSend)(
        gClient, NSSelectorFromString(@"sendWithMessage:freeWhenDone:completionQueue:completion:"),
        msg, YES, gQueue, ^(NSError *e) { failure = e; dispatch_semaphore_signal(sem); });
  } @catch (NSException *ex) {
    fprintf(stderr, "shame-sim: send raised: %s\n", ex.reason.UTF8String);
    gFailed = 1;
    return;
  }
  if (dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC))) {
    fprintf(stderr, "shame-sim: timed out waiting for the simulator to accept the event\n");
    gFailed = 1;
  } else if (failure) {
    fprintf(stderr, "shame-sim: delivery failed: %s\n", failure.description.UTF8String);
    gFailed = 1;
  }
}

// down: YES = contact down (also used for drag moves), NO = lift.
static void touch(double nx, double ny, BOOL down) {
  CGPoint p = CGPointMake(nx, ny);
  IndigoMessage *src = gMouse(&p, NULL, kTouchTarget, down ? 1 : 2, CGSizeMake(1, 1), 0);
  src->payload.event.touch.xRatio = nx;
  src->payload.event.touch.yRatio = ny;
  size_t stride = sizeof(IndigoPayload);
  size_t size = sizeof(IndigoMessage) + stride;
  IndigoMessage *m = calloc(1, size);
  m->innerSize = (unsigned)stride;
  m->eventType = 2; // single touch
  m->payload.eventKind = 0xB;
  m->payload.timestamp = mach_absolute_time();
  memcpy((char *)m + 0x30, (char *)src + 0x30, sizeof(IndigoTouch));
  free(src);
  memcpy((char *)m + 0x20 + stride, (char *)m + 0x20, stride);
  IndigoPayload *second = (IndigoPayload *)((char *)m + 0x20 + stride);
  second->event.touch.field1 = 1;
  second->event.touch.field2 = 2;
  sendMessage(m);
}

static void home(void) {
  sendMessage(gButton(0, 1, kButtonTargetHardware));
  usleep(60 * 1000);
  sendMessage(gButton(0, 2, kButtonTargetHardware));
}


// MARK: - Framebuffer streaming

static id msg0(id obj, const char *sel) {
  SEL s = sel_registerName(sel);
  if (!obj || ![obj respondsToSelector:s]) return nil;
  return ((id(*)(id, SEL))objc_msgSend)(obj, s);
}

// The main display's descriptor: an IO port whose descriptor vends an IOSurface.
static id mainDisplay(void) {
  id io = msg0(gDevice, "io");
  NSArray *ports = msg0(io, "ioPorts");
  id fallback = nil;
  for (id port in ports) {
    id desc = msg0(port, "descriptor");
    if (!desc) continue;
    if (![desc respondsToSelector:sel_registerName("framebufferSurface")] &&
        ![desc respondsToSelector:sel_registerName("ioSurface")]) continue;
    @try {
      id state = msg0(desc, "state");
      if (state && [[state valueForKey:@"displayClass"] intValue] == 0) return desc;
    } @catch (NSException *e) {}
    if (!fallback) fallback = desc;
  }
  return fallback;
}

static IOSurfaceRef currentSurface(id display) {
  id surf = nil;
  @try {
    surf = msg0(display, "framebufferSurface") ?: msg0(display, "ioSurface");
  } @catch (NSException *e) {}
  return (__bridge IOSurfaceRef)surf;
}

static dispatch_source_t gStreamTimer;

static void startStream(void) {
  id display = nil;
  for (int i = 0; i < 50 && !(display = mainDisplay()); i++) usleep(100 * 1000);
  if (!display) { fprintf(stderr, "shame-sim: no display surface (is the device booted?)\n"); return; }
  dispatch_queue_t q = dispatch_queue_create("shame.sim.stream", DISPATCH_QUEUE_SERIAL);
  gStreamTimer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, q);
  // ~30 fps: every frame makes the editor repaint, and 60 is more than a phone mirror needs.
  dispatch_source_set_timer(gStreamTimer, DISPATCH_TIME_NOW, 33 * NSEC_PER_MSEC, 3 * NSEC_PER_MSEC);
  __block IOSurfaceRef lastSurface = NULL;
  __block uint32_t lastSeed = 0;
  __block uint8_t *out = NULL;
  __block size_t outCap = 0;
  dispatch_source_set_event_handler(gStreamTimer, ^{
    @autoreleasepool {
      IOSurfaceRef surf = currentSurface(display);
      if (!surf) return;
      uint32_t seed = IOSurfaceGetSeed(surf);
      if (surf == lastSurface && seed == lastSeed) return;
      if (IOSurfaceLock(surf, kIOSurfaceLockReadOnly, NULL) != 0) return;
      size_t w = IOSurfaceGetWidth(surf), h = IOSurfaceGetHeight(surf);
      size_t stride = IOSurfaceGetBytesPerRow(surf);
      const uint8_t *src = IOSurfaceGetBaseAddress(surf);
      size_t ow = w / 2, oh = h / 2, bytes = ow * oh * 4;
      if (src && ow && oh) {
        if (bytes > outCap) { free(out); out = malloc(bytes); outCap = bytes; }
        // 2x2 box downscale, BGRA.
        for (size_t y = 0; y < oh; y++) {
          const uint8_t *r0 = src + (2 * y) * stride, *r1 = r0 + stride;
          uint8_t *dst = out + y * ow * 4;
          for (size_t x = 0; x < ow; x++) {
            for (int c = 0; c < 4; c++)
              dst[x * 4 + c] = (r0[x * 8 + c] + r0[x * 8 + 4 + c] + r1[x * 8 + c] + r1[x * 8 + 4 + c] + 2) >> 2;
          }
        }
      }
      IOSurfaceUnlock(surf, kIOSurfaceLockReadOnly, NULL);
      lastSurface = surf;
      lastSeed = seed;
      if (!src || !ow || !oh) return;
      uint32_t hdr[4] = { 0x4D524653 /* "SFRM" LE */, (uint32_t)ow, (uint32_t)oh, (uint32_t)bytes };
      if (fwrite(hdr, sizeof hdr, 1, stdout) != 1 || fwrite(out, 1, bytes, stdout) != bytes ||
          fflush(stdout) != 0)
        exit(0); // reader went away
    }
  });
  dispatch_resume(gStreamTimer);
}

static void serve(void) {
  char line[256];
  while (fgets(line, sizeof line, stdin)) {
    @autoreleasepool {
      char verb[16] = {0};
      double x = 0, y = 0;
      int n = sscanf(line, "%15s %lf %lf", verb, &x, &y);
      if (n >= 3 && !strcmp(verb, "down")) touch(x, y, YES);
      else if (n >= 3 && !strcmp(verb, "move")) touch(x, y, YES);
      else if (n >= 3 && !strcmp(verb, "up")) touch(x, y, NO);
      else if (n >= 3 && !strcmp(verb, "tap")) { touch(x, y, YES); usleep(50 * 1000); touch(x, y, NO); }
      else if (n >= 1 && !strcmp(verb, "home")) home();
    }
  }
}

int main(int argc, char **argv) {
  @autoreleasepool {
    if (argc < 3) die(@"usage: shame-sim tap|swipe|home|serve <udid> …");
    NSString *cmd = @(argv[1]), *udid = @(argv[2]);
    setup(udid);
    if ([cmd isEqualToString:@"serve"]) {
      // Video does not need the HID client, so it starts first.
      for (int i = 3; i < argc; i++)
        if (!strcmp(argv[i], "--stream")) startStream();
      // A freshly created HID client drops events sent immediately; give it a moment.
      usleep(400 * 1000);
      serve();
      _exit(0); // stdin closed; don't wait on a stdout reader that may be gone
    } else if ([cmd isEqualToString:@"tap"] && argc >= 5) {
      double x = atof(argv[3]), y = atof(argv[4]);
      touch(x, y, YES);
      usleep(60 * 1000);
      touch(x, y, NO);
    } else if ([cmd isEqualToString:@"swipe"] && argc >= 7) {
      double x1 = atof(argv[3]), y1 = atof(argv[4]), x2 = atof(argv[5]), y2 = atof(argv[6]);
      double ms = argc >= 8 ? atof(argv[7]) : 300;
      int steps = MAX(6, (int)(ms / 16));
      for (int i = 0; i <= steps; i++) {
        double t = (double)i / steps;
        touch(x1 + (x2 - x1) * t, y1 + (y2 - y1) * t, YES);
        usleep((useconds_t)(ms * 1000 / steps));
      }
      touch(x2, y2, NO);
    } else if ([cmd isEqualToString:@"home"]) {
      home();
    } else {
      die(@"bad arguments");
    }
  }
  return gFailed;
}
