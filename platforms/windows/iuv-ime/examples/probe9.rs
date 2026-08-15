//! PE 特征对比：读两个 .ime 的 PE 头特征（Characteristics/Subsystem/DLL 特征/入口点）。
//! 用法：cargo build --target i686-pc-windows-msvc -p iuv-ime --example probe9 --release

fn read_pe(path: &str) {
    let data = std::fs::read(path).unwrap();
    let pe_off = u32::from_le_bytes(data[0x3C..0x40].try_into().unwrap()) as usize;
    let machine = u16::from_le_bytes(data[pe_off + 4..pe_off + 6].try_into().unwrap());
    let num_sec = u16::from_le_bytes(data[pe_off + 6..pe_off + 8].try_into().unwrap());
    let chars = u16::from_le_bytes(data[pe_off + 22..pe_off + 24].try_into().unwrap());
    let opt_size = u16::from_le_bytes(data[pe_off + 20..pe_off + 22].try_into().unwrap());
    let opt = pe_off + 24;
    let magic = u16::from_le_bytes(data[opt..opt + 2].try_into().unwrap());
    let subsystem = u16::from_le_bytes(data[opt + 68..opt + 70].try_into().unwrap());
    let dll_chars = u16::from_le_bytes(data[opt + 70..opt + 72].try_into().unwrap());
    let entry = if magic == 0x20B {
        u32::from_le_bytes(data[opt + 16..opt + 20].try_into().unwrap())
    } else {
        u32::from_le_bytes(data[opt + 16..opt + 20].try_into().unwrap())
    };
    println!("== {path}");
    println!("  machine=0x{machine:04X} sections={num_sec} chars=0x{chars:04X}");
    println!("  magic=0x{magic:04X} subsystem={subsystem} dllchars=0x{dll_chars:04X} entry=0x{entry:08X}");
    let sec_start = opt + opt_size as usize;
    for i in 0..num_sec as usize {
        let s = sec_start + i * 40;
        let name = String::from_utf8_lossy(&data[s..s + 8]);
        let vs = u32::from_le_bytes(data[s + 8..s + 12].try_into().unwrap());
        let va = u32::from_le_bytes(data[s + 12..s + 16].try_into().unwrap());
        let rs = u32::from_le_bytes(data[s + 16..s + 20].try_into().unwrap());
        println!("    sec[{name}] va=0x{va:08X} vs={vs} raw={rs}");
    }
}

fn main() {
    read_pe("C:\\Windows\\SysWOW64\\weasel.ime");
    read_pe("C:\\Windows\\SysWOW64\\iuv.ime");
}
