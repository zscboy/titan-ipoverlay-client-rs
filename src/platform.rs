#[cfg(target_os = "android")]
mod android {
    use std::os::raw::c_int;

    extern "C" {
        fn prctl(option: c_int, arg2: c_int, arg3: c_int, arg4: c_int, arg5: c_int) -> c_int;
    }

    const PR_SET_PDEATHSIG: c_int = 1;
    const SIGKILL: c_int = 9;

    pub fn set_parent_death_signal() {
        unsafe {
            let ret = prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0);
            if ret != 0 {
                eprintln!("prctl(PR_SET_PDEATHSIG) failed: {}", ret);
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
mod non_android {
    pub fn set_parent_death_signal() {
        println!("PDEATHSIG skipped on non-Android platform");
    }
}

// 对外统一导出函数
#[cfg(target_os = "android")]
pub use android::set_parent_death_signal;

#[cfg(not(target_os = "android"))]
pub use non_android::set_parent_death_signal;
