use crossbeam::channel;
use libmpv::SetData as _;
use std::fmt::Formatter;
use std::{collections::VecDeque, ffi, fmt, ptr};

fn mpv_err<T>(ret: T, err: ffi::c_int) -> libmpv::Result<T> {
    if err == 0 {
        Ok(ret)
    } else {
        Err(libmpv::Error::Raw(err))
    }
}

#[derive(Clone, Debug)]
pub enum ClientRequest {
    Exit,
    Completed(u64),
    Command(u64, Vec<String>),
    SetPropertyFlag(u64, String, bool),
}

impl fmt::Display for ClientRequest {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exit => write!(f, "exit"),
            Self::Completed(id) => write!(f, "completed[{id}]"),
            Self::Command(id, cmd) => write!(f, "command[{id}] {}", cmd.join(" ")),
            Self::SetPropertyFlag(id, name, data) => write!(f, "set-property[{id}] {name} {data}"),
        }
    }
}

pub struct MpvOmtClient {
    handle: *mut libmpv_sys::mpv_handle,
    recv: channel::Receiver<ClientRequest>,
    queue: VecDeque<ClientRequest>,
    active: Option<u64>,
}

impl MpvOmtClient {
    pub fn new(
        handle: *mut libmpv_sys::mpv_handle,
        recv: channel::Receiver<ClientRequest>,
    ) -> Self {
        Self {
            handle,
            recv,
            queue: VecDeque::new(),
            active: None,
        }
    }

    pub fn run_until_exit(&mut self) {
        while let Ok(msg) = self.recv.recv() {
            // Take our message out of the channel and put in our todo queue.
            match msg {
                ClientRequest::Exit => break,
                ClientRequest::Completed(id) => {
                    assert_eq!(Some(id), self.active, "completed task that was not active");
                    self.active = None;
                }
                msg => self.queue.push_back(msg),
            }

            // NOTE: MPV does not preserve ordering between tasks, so run commands concurrently
            //       at your own peril! At the same time, as per the docs, we will deadlock if we
            //       just naively block the renderer on core activity. We solve this by only
            //       running a task if none is active. Clearing the active task requires that we
            //       receive the completion on the main thread and process it above.
            if self.active.is_none()
                && let Some(msg) = self.queue.pop_front()
            {
                match msg {
                    ClientRequest::Exit | ClientRequest::Completed(_) => {
                        panic!("got exit/completed command in queue")
                    }
                    ClientRequest::Command(id, cmd) => {
                        self.active = Some(id);
                        self.command_async(id, cmd)
                            .expect("failed to run command async");
                    }
                    ClientRequest::SetPropertyFlag(id, name, data) => {
                        self.active = Some(id);
                        self.set_property_flag_async(id, &name, data)
                            .expect("failed to set prop async");
                    }
                }
            }
        }
    }

    fn command_async(&self, id: u64, mut cmd: Vec<String>) -> libmpv::Result<()> {
        // Allocate a zero-terminated string for each of our inputs, with stack lifetime.
        let mut c_strings: Vec<ffi::CString> = Vec::with_capacity(cmd.len());
        for arg in cmd.drain(..) {
            c_strings.push(ffi::CString::new(arg)?);
        }

        // Create a zero-terminated array of pointers to the strings.
        let mut c_array = c_strings.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
        c_array.push(ptr::null_mut());

        // SAFETY: The call below will parse all the data into Nodes, then act on it. As such,
        //         the CString we pass here only need to be alive for the duration of the call.
        //         Note: also validated with Valgrind.
        mpv_err((), unsafe {
            libmpv_sys::mpv_command_async(self.handle, id, c_array.as_mut_ptr())
        })
    }

    fn set_property_flag_async(&self, id: u64, name: &str, flag: bool) -> libmpv::Result<()> {
        let name = ffi::CString::new(name)?;
        flag.call_as_c_void(|ptr| {
            // SAFETY: like with command_async, these strings get parsed into new containers so it
            //         is safe to free them immediately after the call finishes.
            mpv_err((), unsafe {
                libmpv_sys::mpv_set_property_async(
                    self.handle,
                    id,
                    name.as_ptr(),
                    libmpv::mpv_format::Flag,
                    ptr,
                )
            })
        })
    }
}
