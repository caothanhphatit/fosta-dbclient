use tokio::process::Child;

#[cfg(windows)]
pub fn bind_to_app_lifetime(child: &Child) {
    kill_on_close::assign(child);
}

#[cfg(not(windows))]
pub fn bind_to_app_lifetime(_child: &Child) {}

#[cfg(windows)]
mod kill_on_close {
    use std::{ffi::c_void, mem, ptr, sync::OnceLock};

    use tokio::process::Child;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    struct JobHandle(usize);

    static JOB: OnceLock<Option<JobHandle>> = OnceLock::new();

    pub fn assign(child: &Child) {
        let (Some(job), Some(process)) = (JOB.get_or_init(create_job), child.raw_handle()) else {
            return;
        };
        unsafe {
            AssignProcessToJobObject(job.0 as *mut c_void, process);
        }
    }

    fn create_job() -> Option<JobHandle> {
        unsafe {
            let job = CreateJobObjectW(ptr::null(), ptr::null());
            if job.is_null() {
                return None;
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let applied = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&limits).cast::<c_void>(),
                mem::size_of_val(&limits) as u32,
            );
            (applied != 0).then_some(JobHandle(job as usize))
        }
    }
}
