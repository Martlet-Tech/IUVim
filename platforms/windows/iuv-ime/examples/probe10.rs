//! 导入表对比：解析 PE 导入 DLL 列表（weasel.ime vs iuv.ime）。
//! 用法：cargo build --target i686-pc-windows-msvc -p iuv-ime --example probe10 --release

use std::io::Read;

fn rva_to_offset(data: &[u8], rva: u32, sections: &[(u32, u32)]) -> usize {
    for &(va, vs) in sections {
        if rva >= va && rva < va + vs {
            return (rva - va) as usize;
        }
    }
    rva as usize
}

fn read_imports(path: &str) -> Vec<String> {
    let mut f = std::fs::File::open(path).unwrap();
    let mut data = Vec::new();
    f.read_to_end(&mut data).unwrap();
    let pe_off = u32::from_le_bytes(data[0x3C..0x40].try_into().unwrap()) as usize;
    let num_sec = u16::from_le_bytes(data[pe_off + 6..pe_off + 8].try_into().unwrap()) as usize;
    let opt_size = u16::from_le_bytes(data[pe_off + 20..pe_off + 22].try_into().unwrap()) as usize;
    let opt = pe_off + 24;
    let magic = u16::from_le_bytes(data[opt..opt + 2].try_into().unwrap());
    let dd_off = if magic == 0x20B { opt + 112 } else { opt + 96 };
    let import_rva = u32::from_le_bytes(data[dd_off + 8..dd_off + 12].try_into().unwrap());
    let sec_start = opt + opt_size;
    let mut sections = Vec::new();
    for i in 0..num_sec {
        let s = sec_start + i * 40;
        let vs = u32::from_le_bytes(data[s + 8..s + 12].try_into().unwrap());
        let va = u32::from_le_bytes(data[s + 12..s + 16].try_into().unwrap());
        sections.push((va, vs));
    }
    let mut names = Vec::new();
    if import_rva != 0 {
        let mut off = rva_to_offset(&data, import_rva, &sections);
        loop {
            let name_rva = u32::from_le_bytes(data[off + 12..off + 16].try_into().unwrap());
            if name_rva == 0 {
                break;
            }
            let mut noff = rva_to_offset(&data, name_rva, &sections);
            let mut name = String::new();
            while data[noff] != 0 {
                name.push(data[noff] as char);
                noff += 1;
            }
            names.push(name);
            off += 20;
        }
    }
    names
}

fn main() {
    for p in ["C:\\Windows\\SysWOW64\\weasel.ime", "C:\\Windows\\SysWOW64\\iuv.ime"] {
        println!("== {p}");
        println!("  导入 DLL: {:?}", read_imports(p));
    }
}
