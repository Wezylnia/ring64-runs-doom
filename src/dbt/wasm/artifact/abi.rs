use wasm_encoder::{TypeSection, ValType};

pub(super) fn add_function_type<const PARAMS: usize, const RESULTS: usize>(
    types: &mut TypeSection,
    params: [ValType; PARAMS],
    results: [ValType; RESULTS],
) -> u32 {
    let index = types.len();
    types.ty().function(params, results);
    index
}

pub(super) struct BaseSignatures {
    pub(super) load: u32,
    pub(super) store: u32,
    pub(super) status: u32,
    pub(super) run: u32,
    pub(super) target: Option<u32>,
}

impl BaseSignatures {
    pub(super) fn declare(types: &mut TypeSection, segmented: bool, target: bool) -> Self {
        use ValType::{I32, I64};
        let load = if segmented {
            add_function_type(types, [I64, I32, I32], [I64])
        } else {
            add_function_type(types, [I64, I32], [I64])
        };
        let store = if segmented {
            add_function_type(types, [I64, I32, I64, I32], [I32])
        } else {
            add_function_type(types, [I64, I32, I64], [I32])
        };
        let status = add_function_type(types, [], [I32]);
        let run = add_function_type(types, [I32, I64, I64, I32, I32], [I32]);
        let target = target.then(|| add_function_type(types, [I64], [I32]));
        Self {
            load,
            store,
            status,
            run,
            target,
        }
    }
}
