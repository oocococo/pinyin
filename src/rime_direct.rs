use std::{mem, os::raw::c_int};

use anyhow::{bail, Result};
use librime_sys::{RimeApi, RimeSessionId};

type ChangePageFn = unsafe extern "C" fn(RimeSessionId, c_int) -> c_int;
type SelectCurrentPageFn = unsafe extern "C" fn(RimeSessionId, usize) -> c_int;

pub fn change_page(session_id: RimeSessionId, backward: bool) -> Result<bool> {
    let api = api()?;
    ensure_field_available(
        api,
        mem::offset_of!(RimeApi, change_page),
        mem::size_of::<Option<ChangePageFn>>(),
        "change_page",
    )?;
    let function = unsafe { (*api).change_page }
        .ok_or_else(|| anyhow::anyhow!("librime API change_page is unavailable"))?;
    Ok(unsafe { function(session_id, c_int::from(backward)) } != 0)
}

pub fn select_candidate_on_current_page(session_id: RimeSessionId, index: usize) -> Result<bool> {
    let api = api()?;
    ensure_field_available(
        api,
        mem::offset_of!(RimeApi, select_candidate_on_current_page),
        mem::size_of::<Option<SelectCurrentPageFn>>(),
        "select_candidate_on_current_page",
    )?;
    let function = unsafe { (*api).select_candidate_on_current_page }.ok_or_else(|| {
        anyhow::anyhow!("librime API select_candidate_on_current_page is unavailable")
    })?;
    Ok(unsafe { function(session_id, index) } != 0)
}

fn api() -> Result<*mut RimeApi> {
    let api = unsafe { librime_sys::rime_get_api() };
    if api.is_null() {
        bail!("librime returned a null API table")
    }
    Ok(api)
}

fn ensure_field_available(
    api: *const RimeApi,
    field_offset: usize,
    field_size: usize,
    field_name: &str,
) -> Result<()> {
    let data_size = unsafe { api.cast::<c_int>().read() };
    if data_size < 0 {
        bail!("librime API table has an invalid negative data_size: {data_size}")
    }
    let available = data_size as usize + mem::size_of::<c_int>();
    let required = field_offset.saturating_add(field_size);
    if required > available {
        bail!(
            "librime API {field_name} requires {required} table bytes, but this version exposes {available}"
        )
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_bindings_place_candidate_functions_inside_the_api_table() {
        let select_end = mem::offset_of!(RimeApi, select_candidate_on_current_page)
            + mem::size_of::<Option<SelectCurrentPageFn>>();
        let page_end =
            mem::offset_of!(RimeApi, change_page) + mem::size_of::<Option<ChangePageFn>>();

        assert!(select_end <= mem::size_of::<RimeApi>());
        assert!(page_end <= mem::size_of::<RimeApi>());
    }
}
