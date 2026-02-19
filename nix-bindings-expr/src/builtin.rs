use crate::eval_state::EvalState;
use crate::value::Value;
use anyhow::Result;
use nix_bindings_expr_sys as raw;
use nix_bindings_util::check_call;
use nix_bindings_util_sys as raw_util;
use std::ffi::{c_int, c_void, CString};
use std::mem::ManuallyDrop;
use std::ptr::{null, null_mut};

pub struct Builtin {
    pub(crate) ptr: *mut raw::PrimOp,
}
impl Drop for Builtin {
    fn drop(&mut self) {
        unsafe {
            raw::gc_decref(null_mut(), self.ptr as *mut c_void);
        }
    }
}
impl Builtin {
    pub fn new<const N: usize>(
        mut context: nix_bindings_util::context::Context,
        meta: crate::primop::PrimOpMeta<N>,
        f: Box<dyn Fn(&mut EvalState, &[Value; N]) -> Result<Value>>,
    ) -> Result<()> {
        assert!(N != 0);

        let mut args = Vec::new();
        for arg in meta.args {
            args.push(arg.as_ptr());
        }
        args.push(null());

        // Primops weren't meant to be dynamically created, as of writing.
        // This leaks, and so do the primop fields in Nix internally.
        let user_data = {
            // We'll be leaking this Box.
            // TODO: Use the GC with finalizer, if possible.
            let user_data = ManuallyDrop::new(Box::new(BuiltinContext {
                arity: N,
                function: Box::new(move |eval_state, args| f(eval_state, args.try_into().unwrap())),
            }));
            user_data.as_ref() as *const BuiltinContext as *mut c_void
        };
        let op = unsafe {
            check_call!(raw::alloc_primop(
                &mut context,
                FUNCTION_ADAPTER,
                N as c_int,
                meta.name.as_ptr(),
                args.as_mut_ptr(), /* TODO add an extra const to bindings to avoid mut here. */
                meta.doc.as_ptr(),
                user_data
            ))?
        };

        unsafe {
            check_call!(raw::register_primop(&mut context, op))?;
            check_call!(raw::gc_decref(&mut context, op as *mut c_void))?;
        };

        Ok(())
    }
}

/// The user_data for our Nix builtin
struct BuiltinContext {
    arity: usize,
    function: Box<dyn Fn(&mut EvalState, &[Value]) -> Result<Value>>,
}

unsafe extern "C" fn function_adapter(
    user_data: *mut ::std::os::raw::c_void,
    context_out: *mut raw_util::c_context,
    eval_state_raw: *mut raw::EvalState,
    args: *mut *mut raw::Value,
    ret: *mut raw::Value,
) {
    let primop_info = (user_data as *const BuiltinContext).as_ref().unwrap();
    let args_raw_slice = unsafe { std::slice::from_raw_parts(args, primop_info.arity) };
    let args_vec: Vec<Value> = args_raw_slice
        .iter()
        .map(|v| Value::new_borrowed(*v))
        .collect();
    let args_slice = args_vec.as_slice();

    let mut eval_state = eval_state_raw.into();
    let r = primop_info.function.as_ref()(&mut eval_state, args_slice);

    match r {
        Ok(v) => unsafe {
            raw::copy_value(context_out, ret, v.raw_ptr());
        },
        Err(e) => unsafe {
            let cstr = CString::new(e.to_string()).unwrap_or_else(|_e| {
                CString::new("<rust nix-expr application error message contained null byte>")
                    .unwrap()
            });
            raw_util::set_err_msg(context_out, raw_util::err_NIX_ERR_UNKNOWN, cstr.as_ptr());
        },
    }
}

static FUNCTION_ADAPTER: raw::PrimOpFun = Some(function_adapter);
