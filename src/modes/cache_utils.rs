use hex::{decode, encode};
use sha2::{Digest, Sha256};
use std::panic;

/// 将整数token序列转换为展平的二元组（bigram）一维数组
/// 输入: [1, 2, 3, 4] -> 输出: [1, 2, 2, 3, 3, 4]
/// 输入长度 < 2 时返回空数组
pub fn convert_to_bigram_key(tokens: &[i32]) -> Vec<i32> {
    // 长度不足2，直接返回空向量
    if tokens.len() < 2 {
        return Vec::new();
    }

    let mut result = Vec::new();
    // 遍历所有相邻元素对，依次推入两个元素（直接展平）
    for i in 0..tokens.len() - 1 {
        result.push(tokens[i]);
        result.push(tokens[i + 1]);
    }

    result
}

/// 与Python版本完全一致的SHA256哈希函数
/// - token_ids: 无符号32位整数列表（对应Python的int，需符合4字节无符号范围）
/// - prior_hash: 可选的十六进制前缀哈希字符串（无效会panic，与Python行为一致）
/// - 返回：小写的SHA256哈希十六进制字符串
pub fn get_hash_str(token_ids: &[u32], prior_hash: Option<&str>) -> String {
    // 初始化SHA256哈希器
    let mut hasher = Sha256::new();

    // 处理前缀哈希（与Python的bytes.fromhex逻辑一致）
    if let Some(ph) = prior_hash {
        let ph_bytes = decode(ph).unwrap_or_else(|e| panic!("Invalid prior_hash hex string: {}", e));
        hasher.update(ph_bytes);
    }

    // 遍历token_ids，转为4字节小端序并更新哈希器
    for &token in token_ids {
        // u32.to_le_bytes() 等价于Python的t.to_bytes(4, "little", signed=False)
        let token_bytes = token.to_le_bytes();
        hasher.update(token_bytes);
    }

    // 生成最终哈希（hex::encode默认大写，转小写对齐Python的hexdigest）
    let hash_result = hasher.finalize();
    encode(hash_result).to_lowercase()
}

// 验证与Python输出一致性的测试用例
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_hash_str_basic() {
        // 测试用例1：无prior_hash，简单token_ids
        let token_ids = &[1, 2, 3];
        let rust_hash = get_hash_str(token_ids, None);
        // SHA256 of [1,0,0,0, 2,0,0,0, 3,0,0,0] (little endian u32 bytes)
        assert_eq!(
            rust_hash,
            "4636993d3e1da4e9d6b8f87b79e8f7c6d018580d52661950eabc3845c5897a4d"
        );
        assert_eq!(rust_hash.len(), 64); // SHA256 produces 256 bits = 64 hex chars
    }

    #[test]
    fn test_get_hash_str_with_prior() {
        // 测试用例2：带prior_hash的场景
        let token_ids = &[1, 2, 3];
        let prior_hash = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let rust_hash_with_prior = get_hash_str(token_ids, Some(prior_hash));
        // Should produce a valid SHA256 hash (64 hex characters)
        assert_eq!(rust_hash_with_prior.len(), 64);
    }

    #[test]
    fn test_get_hash_str_empty() {
        // 测试空token_ids
        let token_ids: &[u32] = &[];
        let rust_hash = get_hash_str(token_ids, None);
        // SHA256 of empty input
        assert_eq!(
            rust_hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
