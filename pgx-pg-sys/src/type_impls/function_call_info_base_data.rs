use crate::{Datum, FunctionCallInfoBaseData, PgPtr};

impl PgPtr<FunctionCallInfoBaseData> {
    pub fn get_arg_datum(&self, i: i16) -> Option<Datum> {
        if i < 0 || i >= self.nargs {
            None
        } else {
            unsafe {
                let nd = &self.args.as_slice(self.nargs as usize)[i as usize];
                if nd.isnull {
                    None
                } else {
                    Some(nd.value)
                }
            }
        }
    }
}
