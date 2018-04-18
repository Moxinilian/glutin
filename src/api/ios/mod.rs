//! iOS support
//!
//! # Building app
//! To build ios app you will need rustc built for this targets:
//!
//!  - armv7-apple-ios
//!  - armv7s-apple-ios
//!  - i386-apple-ios
//!  - aarch64-apple-ios
//!  - x86_64-apple-ios
//!
//! Then
//!
//! ```
//! cargo build --target=...
//! ```
//! The simplest way to integrate your app into xcode environment is to build it
//! as a static library. Wrap your main function and export it.
//!
//! ```rust, ignore
//! #[no_mangle]
//! pub extern fn start_glutin_app() {
//!     start_inner()
//! }
//!
//! fn start_inner() {
//!    ...
//! }
//!
//! ```
//!
//! Compile project and then drag resulting .a into Xcode project. Add glutin.h to xcode.
//!
//! ```c
//! void start_glutin_app();
//! ```
//!
//! Use start_glutin_app inside your xcode's main function.
//!
//!
//! # App lifecycle and events
//!
//! iOS environment is very different from other platforms and you must be very
//! careful with it's events. Familiarize yourself with [app lifecycle](https://developer.apple.com/library/ios/documentation/UIKit/Reference/UIApplicationDelegate_Protocol/).
//!
//!
//! This is how those event are represented in glutin:
//!
//!  - applicationDidBecomeActive is Focused(true)
//!  - applicationWillResignActive is Focused(false)
//!  - applicationDidEnterBackground is Suspended(true)
//!  - applicationWillEnterForeground is Suspended(false)
//!  - applicationWillTerminate is Closed
//!
//! Keep in mind that after Closed event is received every attempt to draw with opengl will result in segfault.
//!
//! Also note that app will not receive Closed event if suspended, it will be SIGKILL'ed

#![cfg(target_os = "ios")]
#![deny(warnings)]

use winit;
use PixelFormatRequirements;
use GlAttributes;
use CreationError;
use WindowAttributes;

use std::os::raw::c_void;
use std::io;

mod ffi;
use self::ffi::{dlopen, dlsym, gles, id, nil, setjmp, CFRunLoopRunInMode, CFTimeInterval, CGFloat,
                NSString, UIApplicationMain, kCFRunLoopDefaultMode, kCFRunLoopRunHandledSource,
                kEAGLColorFormatRGB565, kEAGLDrawablePropertyColorFormat,
                kEAGLDrawablePropertyRetainedBacking, RTLD_GLOBAL, RTLD_LAZY};

use objc::runtime::{Class, BOOL, NO, YES};

const VIEW_CLASS: &'static str = "MainView";

pub struct Context {
    eagl_context: id,
    view: id,
}

impl Context {
    pub fn new(
        window_builder: winit::WindowBuilder,
        events_loop: &winit::EventsLoop,
        pf_reqs: &PixelFormatRequirements,
        gl_attr: &GlAttributes<&Self>,
    ) -> Result<(winit::Window, Self), CreationError> {
        let window = try!(window_builder.build(events_loop));
        let eagl_ctx = Context::create_context();

        create_uiview_class();
        unsafe {
            let app: id = msg_send![Class::get("UIApplication").unwrap(), sharedApplication]; // NOTE: Isn't that just `shared`?
            let delegate: id = msg_send![app, delegate];
            let state: *mut libc::c_void = *(&*delegate).get_ivar("glutinState");
            let state = state as *mut DelegateState;

            let main_screen: id = msg_send![Class::get("UIScreen").unwrap(), mainScreen];
            let bounds: CGRect = msg_send![main_screen, bounds];

            let class = Class::get(VIEW_CLASS).unwrap();
            let view: id = msg_send![class, alloc];
            let view: id = msg_send![view, initForGl: &bounds];

            let _: () = msg_send![state.controller, setView:view];
            let _: () = msg_send![state.window, addSubview:view];

            let mut ctx = Context {
                eagl_context: eagl_ctx,
                view: view,
            };

            ctx.init_context(&builder.window, &state);
            Ok((window, ctx))
        }
    }

    unsafe fn init_context(&mut self, builder: &WindowAttributes, state: &DelegateState) {
        let draw_props: id = msg_send![Class::get("NSDictionary").unwrap(), alloc];
        let draw_props: id = msg_send![draw_props,
                    initWithObjects:
                        vec![
                            msg_send![Class::get("NSNumber").unwrap(), numberWithBool: NO],
                            kEAGLColorFormatRGB565
                        ].as_ptr()
                    forKeys:
                        vec![
                            kEAGLDrawablePropertyRetainedBacking,
                            kEAGLDrawablePropertyColorFormat
                        ].as_ptr()
                    count: 2
            ];
        let _ = self.make_current();

        if builder.multitouch {
            let _: () = msg_send![state.view, setMultipleTouchEnabled: YES];
        }

        let _: () = msg_send![state.view, setContentScaleFactor:state.scale as CGFloat];

        let layer: id = msg_send![state.view, layer];
        let _: () = msg_send![layer, setContentsScale:state.scale as CGFloat];
        let _: () = msg_send![layer, setDrawableProperties: draw_props];

        let gl = gles::Gles2::load_with(|symbol| self.get_proc_address(symbol) as *const c_void);
        let mut color_render_buf: gles::types::GLuint = 0;
        let mut frame_buf: gles::types::GLuint = 0;
        gl.GenRenderbuffers(1, &mut color_render_buf);
        gl.BindRenderbuffer(gles::RENDERBUFFER, color_render_buf);

        let ok: BOOL =
            msg_send![self.eagl_context, renderbufferStorage:gles::RENDERBUFFER fromDrawable:layer];
        if ok != YES {
            panic!("EAGL: could not set renderbufferStorage");
        }

        gl.GenFramebuffers(1, &mut frame_buf);
        gl.BindFramebuffer(gles::FRAMEBUFFER, frame_buf);

        gl.FramebufferRenderbuffer(
            gles::FRAMEBUFFER,
            gles::COLOR_ATTACHMENT0,
            gles::RENDERBUFFER,
            color_render_buf,
        );

        let status = gl.CheckFramebufferStatus(gles::FRAMEBUFFER);
        if gl.CheckFramebufferStatus(gles::FRAMEBUFFER) != gles::FRAMEBUFFER_COMPLETE {
            panic!("framebuffer status: {:?}", status);
        }
    }

    fn create_context() -> id {
        unsafe {
            let eagl_context: id = msg_send![Class::get("EAGLContext").unwrap(), alloc];
            let eagl_context: id = msg_send![eagl_context, initWithAPI:2]; // es2
            eagl_context
        }
    }

    #[inline]
    unsafe fn make_current(&self) -> Result<(), ContextError> {
        let res: BOOL =
            msg_send![Class::get("EAGLContext").unwrap(), setCurrentContext: self.eagl_context];
        if res == YES {
            Ok(())
        } else {
            Err(ContextError::IoError(io::Error::new(
                io::ErrorKind::Other,
                "EAGLContext::setCurrentContext unsuccessful",
            )))
        }
    }

    pub fn get_proc_address(&self, addr: &str) -> *const () {
        let addr_c = CString::new(addr).unwrap();
        let path = CString::new("/System/Library/Frameworks/OpenGLES.framework/OpenGLES").unwrap();
        unsafe {
            let lib = dlopen(path.as_ptr(), RTLD_LAZY | RTLD_GLOBAL);
            dlsym(lib, addr_c.as_ptr()) as *const _
        }
    }
}

static BUILD_ONCE: bool = false;
fn create_uiview_class() {
    if BUILD_ONCE {
        return;
    }
    BUILD_ONCE = true;

    extern "C" fn init_for_gl(this: &Object, _: Sel, frame: *const libc::c_void) -> id {
        unsafe {
            let bounds: *const CGRect = mem::transmute(frame);
            let view: id = msg_send![this, initWithFrame:(*bounds).clone()];

            let _: () = msg_send![
                view,
                setAutoresizingMask: UIViewAutoresizingFlexibleWidth
                    | UIViewAutoresizingFlexibleHeight
            ];
            let _: () = msg_send![view, setAutoresizesSubviews: YES];

            let layer: id = msg_send![view, layer];
            let _: () = msg_send![layer, setOpaque: YES];

            view
        }
    }

    extern "C" fn layer_class(_: &Class, _: Sel) -> *const Class {
        unsafe { mem::transmute(Class::get("CAEAGLLayer").unwrap()) }
    }

    let superclass = Class::get("UIView").unwrap();
    let mut decl = ClassDecl::new(superclass, VIEW_CLASS).unwrap();

    unsafe {
        decl.add_method(
            sel!(initForGl:),
            init_for_gl as extern "C" fn(&Object, Sel, *const libc::c_void) -> id,
        );

        decl.add_class_method(
            sel!(layerClass),
            layer_class as extern "C" fn(&Class, Sel) -> *const Class,
        );
        decl.register();
    }
}
