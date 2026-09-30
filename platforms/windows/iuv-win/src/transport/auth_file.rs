//! 共享密钥文件（49 §4.4 认证，拍板 §6.4）：安装/首次运行时生成，客户端与服务端
//! 共读同一 token。位置由调用方传入（用户配置目录），文件 ACL 随用户 profile 目录
//! 继承（仅当前用户 + SYSTEM/管理员可读）；显式 DACL 收紧属安装器范畴，后置。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};

use iuv_proto::Auth;

/// token 文件名（置于调用方给定的配置目录下）。
pub fn token_path(dir: &Path) -> PathBuf {
    dir.join("service-token")
}

/// 读取或创建共享密钥。文件已存在 → 读 32 字节（长度非法 = 文件损坏，报错不静默重建
/// ——重建会让已装客户端全部认证失败，必须显式处理）；不存在 → 生成并 `create_new`
/// 写入（并发创建只有一方成功，败方回读胜方的文件，保证两端读到同一个 token）。
pub fn load_or_create_token(dir: &Path) -> io::Result<Auth> {
    fs::create_dir_all(dir)?;
    let path = token_path(dir);
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => {
            let auth = Auth(generate()?);
            use std::io::Write;
            f.write_all(&auth.0)?;
            f.flush()?;
            Ok(auth)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => read_token(&path),
        Err(e) => Err(e),
    }
}

fn read_token(path: &Path) -> io::Result<Auth> {
    let bytes = fs::read(path)?;
    if bytes.len() != 32 {
        return Err(io::Error::other(format!(
            "service-token 长度非法: {} 字节（应为 32），文件可能已损坏",
            bytes.len()
        )));
    }
    let mut a = [0u8; 32];
    a.copy_from_slice(&bytes);
    Ok(Auth(a))
}

/// 32 字节 CSPRNG（系统首选算法，BCryptGenRandom）。
fn generate() -> io::Result<[u8; 32]> {
    let mut buf = [0u8; 32];
    // SAFETY: buf 切片可写；系统首选 RNG 无句柄依赖。NTSTATUS < 0 = 失败。
    let st = unsafe { BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if st.0 < 0 {
        return Err(io::Error::other(format!("BCryptGenRandom 失败: {st:?}")));
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_or_create_is_stable_and_validates_length() {
        // 本测试写 %TEMP%——环境拦截（os error 5）时跳过而非误报（见 status.md 环境条目）。
        let dir = std::env::temp_dir().join(format!("iuv-token-test-{}", std::process::id()));
        if fs::create_dir_all(&dir).is_err() {
            return;
        }
        let first = match load_or_create_token(&dir) {
            Ok(a) => a,
            Err(e) if e.raw_os_error() == Some(5) => return, // 环境受限
            Err(e) => panic!("创建 token 失败: {e}"),
        };
        let again = load_or_create_token(&dir).expect("二次读取应返回同一 token");
        assert_eq!(first, again, "同目录两次 load_or_create 应稳定");
        let raw = fs::read(token_path(&dir)).unwrap();
        assert_eq!(raw.len(), 32);
        let _ = fs::remove_dir_all(&dir);
    }
}
