use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub struct Process(pub Option<Child>);

impl Process {
    pub fn spawn(command: &mut Command) -> Self {
        Self(Some(
            command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ))
    }

    pub fn output(mut self, timeout: Duration) -> Result<Output, Output> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                return Ok(self.0.take().unwrap().wait_with_output().unwrap());
            }
            if Instant::now() >= deadline {
                self.0.as_mut().unwrap().kill().unwrap();
                return Err(self.0.take().unwrap().wait_with_output().unwrap());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn output(command: &mut Command) -> Output {
    Process::spawn(command)
        .output(Duration::from_secs(5))
        .unwrap_or_else(|output| {
            panic!(
                "child exceeded 5 seconds; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
}
