//! macOS Darwin Grand Central Dispatch / pthread QoS class bindings.
//! On Apple Silicon (such as Apple M4), QoS controls whether threads execute
//! on high-IPC Performance Cores (e.g. QOS_CLASS_USER_INTERACTIVE for real-time order execution)
//! or energy-efficient Efficiency Cores (e.g. QOS_CLASS_UTILITY / QOS_CLASS_BACKGROUND for continuous
//! background learning and sensory self-discovery).

#[cfg(target_os = "macos")]
pub mod qos {
    pub const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21; // P-cores: real-time latency critical
    pub const QOS_CLASS_USER_INITIATED: u32 = 0x19;   // P-cores: high-priority user tasks
    pub const QOS_CLASS_DEFAULT: u32 = 0x15;          // Standard system scheduler
    pub const QOS_CLASS_UTILITY: u32 = 0x11;          // E-cores: continuous throughput, no throttle
    pub const QOS_CLASS_BACKGROUND: u32 = 0x09;       // E-cores: background maintenance

    unsafe extern "C" {
        fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
    }

    /// Assign current thread to Apple Silicon Performance Cores (User Interactive).
    pub fn set_interactive() -> bool {
        unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) == 0 }
    }

    /// Assign current thread to Apple Silicon High-Priority (User Initiated).
    pub fn set_user_initiated() -> bool {
        unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INITIATED, 0) == 0 }
    }

    /// Assign current thread to Apple Silicon Efficiency Cores (Utility).
    pub fn set_utility() -> bool {
        unsafe { pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0) == 0 }
    }

    /// Assign current thread to Apple Silicon Background Efficiency Cores.
    pub fn set_background() -> bool {
        unsafe { pthread_set_qos_class_self_np(QOS_CLASS_BACKGROUND, 0) == 0 }
    }
}

#[cfg(not(target_os = "macos"))]
pub mod qos {
    #[cfg(target_os = "linux")]
    fn set_nice(niceness: i32) -> bool {
        unsafe extern "C" {
            fn setpriority(which: i32, who: u32, prio: i32) -> i32;
        }
        // SAFETY: PRIO_PROCESS=0, who=0 sets the calling thread's scheduling priority on Linux.
        unsafe { setpriority(0, 0, niceness) == 0 }
    }

    pub fn set_interactive() -> bool {
        #[cfg(target_os = "linux")]
        {
            set_nice(0)
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    pub fn set_user_initiated() -> bool {
        #[cfg(target_os = "linux")]
        {
            set_nice(0)
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    pub fn set_utility() -> bool {
        #[cfg(target_os = "linux")]
        {
            set_nice(10)
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    pub fn set_background() -> bool {
        #[cfg(target_os = "linux")]
        {
            set_nice(19)
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_darwin_qos() {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            use super::qos::*;
            assert!(set_interactive());
            assert!(set_utility());
        }
    }
}
