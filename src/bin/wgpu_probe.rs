use iced::wgpu;
use std::env;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

fn main() {
    println!("WAYLAND_DISPLAY={}", env::var("WAYLAND_DISPLAY").unwrap_or_default());
    println!("DISPLAY={}", env::var("DISPLAY").unwrap_or_default());
    println!(
        "ICED_BACKEND={}",
        env::var("ICED_BACKEND").unwrap_or_else(|_| "<unset>".to_string())
    );
    println!(
        "WGPU_BACKEND={}",
        env::var("WGPU_BACKEND").unwrap_or_else(|_| "<unset>".to_string())
    );

    let enabled = wgpu::Instance::enabled_backend_features();
    println!("enabled_backend_features={enabled:?}");

    for (label, backends) in [
        ("all", wgpu::Backends::all()),
        ("vulkan", wgpu::Backends::VULKAN),
        ("gl", wgpu::Backends::GL),
    ] {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });

        let adapters = instance.enumerate_adapters(backends);
        println!("{label}: {} adapters", adapters.len());

        for adapter in &adapters {
            let info = adapter.get_info();
            println!(
                "  backend={:?} type={:?} name={} driver={} driver_info={}",
                info.backend, info.device_type, info.name, info.driver, info.driver_info
            );
        }

        let request = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }));

        match request {
            Ok(adapter) => {
                let info = adapter.get_info();
                println!(
                    "  request_adapter: backend={:?} type={:?} name={}",
                    info.backend, info.device_type, info.name
                );
            }
            Err(error) => {
                println!("  request_adapter error: {error:?}");
            }
        }
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = dummy_waker();
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);

    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

fn dummy_waker() -> Waker {
    unsafe fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }

    unsafe fn wake(_: *const ()) {}
    unsafe fn wake_by_ref(_: *const ()) {}
    unsafe fn drop(_: *const ()) {}

    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);

    let raw = RawWaker::new(std::ptr::null(), &VTABLE);

    unsafe { Waker::from_raw(raw) }
}
