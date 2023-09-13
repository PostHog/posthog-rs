use crate::event::Event;
use crate::types::APIResult;
use crate::Client;

pub trait PublicAPI {
    fn capture(&self, event: Event) -> APIResult<()>;
    fn capture_batch(&self, events: Vec<Event>) -> APIResult<()> {
        for event in events {
            self.capture(event)?;
        }
        Ok(())
    }
}

impl PublicAPI for Client {
    fn capture(&self, event: Event) -> APIResult<()> {
        let _res = self.post_request_with_body("/capture/".into(), event);
        Ok(())
    }
}
