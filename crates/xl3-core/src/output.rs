//! Output buffer assembly.
//!
//! Phase 1 P1-A scope: take rendered rows of `Value`s and write them
//! into a fresh `rust_xlsxwriter::Workbook`, save to an in-memory buffer.
//! No style / merge / formula preservation yet — that's the job of the
//! manifest layer in later milestones.

use anyhow::Result;

use crate::rust_xlsxwriter::Workbook;
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct RenderedSheet {
    pub name: String,
    pub rows: Vec<Vec<Value>>,
}

pub fn write_workbook(sheets: &[RenderedSheet]) -> Result<Vec<u8>> {
    let mut wb = Workbook::new();
    for sheet in sheets {
        let ws = wb.add_worksheet();
        ws.set_name(&sheet.name)?;
        for (r, row) in sheet.rows.iter().enumerate() {
            for (c, value) in row.iter().enumerate() {
                let r32 = r as u32;
                let c16 = c as u16;
                match value {
                    Value::Empty => {}
                    Value::String(s) => {
                        ws.write_string(r32, c16, s)?;
                    }
                    Value::Number(n) => {
                        ws.write_number(r32, c16, *n)?;
                    }
                    Value::Bool(b) => {
                        ws.write_boolean(r32, c16, *b)?;
                    }
                }
            }
        }
    }
    Ok(wb.save_to_buffer()?)
}
