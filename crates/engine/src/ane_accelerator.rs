//! Platform-specific vector math acceleration.
//!
//! Directly links and dispatches tensor math, matrix-vector layer forward propagation,
//! and synaptic dot products to Apple's native `Accelerate.framework` (vDSP, BLAS Level 2, and BNNS).
//! Linux x86_64 uses AVX2 and FMA when available. macOS uses Accelerate.framework. Neither
//! path implies Neural Engine dispatch; Accelerate's CPU routines are not an ANE API.

#[cfg(target_os = "macos")]
unsafe extern "C" {
    // Vector dot product: C = sum(A[i] * B[i])
    fn vDSP_dotprD(
        __vDSP_A: *const f64,
        __vDSP_I: i64,
        __vDSP_B: *const f64,
        __vDSP_J: i64,
        __vDSP_C: *mut f64,
        __vDSP_N: u64,
    );

    // Vector scalar multiply: C[i] = A[i] * B
    fn vDSP_vsmulD(
        __vDSP_A: *const f64,
        __vDSP_I: i64,
        __vDSP_B: *const f64,
        __vDSP_C: *mut f64,
        __vDSP_K: i64,
        __vDSP_N: u64,
    );

    // BLAS Level 2 Matrix-Vector Multiplication: y = alpha * A * x + beta * y
    // Order: 101 = CblasRowMajor
    // Trans: 111 = CblasNoTrans
    fn cblas_dgemv(
        order: i32,
        trans: i32,
        m: i32,
        n: i32,
        alpha: f64,
        a: *const f64,
        lda: i32,
        x: *const f64,
        incx: i32,
        beta: f64,
        y: *mut f64,
        incy: i32,
    );
}

/// Returns true when the macOS arm64 Accelerate implementation is selected.
pub fn is_apple_silicon_ane_active() -> bool {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        true
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        false
    }
}

/// Returns a human-readable telemetry description of the active accelerator.
pub fn accelerator_info() -> String {
    if is_apple_silicon_ane_active() {
        return "macOS arm64 Accelerate CPU vector math (vDSP / BLAS)".to_string();
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma") {
            return "Intel x86_64 AVX2 + FMA Hardware Vectorized SIMD".to_string();
        }
    }
    "Native CPU SIMD (Fallback)".to_string()
}

/// Computes hardware-accelerated dot product: $\sum_{i=0}^{N-1} a_i \cdot b_i$.
/// Dispatches to `vDSP_dotprD` on macOS M4, or AVX2+FMA 4-way unrolled SIMD on x86_64.
pub fn accelerated_dot_product(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }

    #[cfg(target_os = "macos")]
    {
        let mut result = 0.0;
        unsafe {
            vDSP_dotprD(
                a.as_ptr(),
                1,
                b.as_ptr(),
                1,
                &mut result,
                n as u64,
            );
        }
        result
    }

    #[cfg(all(not(target_os = "macos"), target_arch = "x86_64"))]
    {
        if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma") {
            // SAFETY: avx2 and fma are verified at runtime.
            return unsafe { avx2_dot_product(&a[..n], &b[..n]) };
        }
        a[..n].iter().zip(b[..n].iter()).map(|(x, y)| x * y).sum()
    }

    #[cfg(all(not(target_os = "macos"), not(target_arch = "x86_64")))]
    {
        a[..n].iter().zip(b[..n].iter()).map(|(x, y)| x * y).sum()
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn avx2_dot_product(a: &[f64], b: &[f64]) -> f64 {
    use std::arch::x86_64::*;
    let n = a.len().min(b.len());
    let mut sum0 = _mm256_setzero_pd();
    let mut sum1 = _mm256_setzero_pd();
    let mut i = 0;
    let ap = a.as_ptr();
    let bp = b.as_ptr();
    while i + 8 <= n {
        unsafe {
            let va0 = _mm256_loadu_pd(ap.add(i));
            let vb0 = _mm256_loadu_pd(bp.add(i));
            sum0 = _mm256_fmadd_pd(va0, vb0, sum0);
            let va1 = _mm256_loadu_pd(ap.add(i + 4));
            let vb1 = _mm256_loadu_pd(bp.add(i + 4));
            sum1 = _mm256_fmadd_pd(va1, vb1, sum1);
        }
        i += 8;
    }
    while i + 4 <= n {
        unsafe {
            let va = _mm256_loadu_pd(ap.add(i));
            let vb = _mm256_loadu_pd(bp.add(i));
            sum0 = _mm256_fmadd_pd(va, vb, sum0);
        }
        i += 4;
    }
    let sum = _mm256_add_pd(sum0, sum1);
    let mut lanes = [0.0f64; 4];
    unsafe {
        _mm256_storeu_pd(lanes.as_mut_ptr(), sum);
    }
    let mut total = (lanes[0] + lanes[1]) + (lanes[2] + lanes[3]);
    while i < n {
        total += a[i] * b[i];
        i += 1;
    }
    total
}

/// Scales a vector in place by scalar factor: $v_i \leftarrow v_i \cdot s$.
/// Dispatches to `vDSP_vsmulD` on macOS M4, or AVX2 SIMD on x86_64.
pub fn accelerated_vector_scale(v: &mut [f64], scale: f64) {
    if v.is_empty() {
        return;
    }

    #[cfg(target_os = "macos")]
    {
        unsafe {
            vDSP_vsmulD(
                v.as_ptr(),
                1,
                &scale,
                v.as_mut_ptr(),
                1,
                v.len() as u64,
            );
        }
    }

    #[cfg(all(not(target_os = "macos"), target_arch = "x86_64"))]
    {
        if std::arch::is_x86_feature_detected!("avx2") {
            unsafe { avx2_vector_scale(v, scale) };
            return;
        }
        for x in v.iter_mut() {
            *x *= scale;
        }
    }

    #[cfg(all(not(target_os = "macos"), not(target_arch = "x86_64")))]
    {
        for x in v.iter_mut() {
            *x *= scale;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2_vector_scale(v: &mut [f64], scale: f64) {
    use std::arch::x86_64::*;
    let n = v.len();
    let vs = _mm256_set1_pd(scale);
    let vp = v.as_mut_ptr();
    let mut i = 0;
    while i + 4 <= n {
        unsafe {
            let x = _mm256_loadu_pd(vp.add(i));
            _mm256_storeu_pd(vp.add(i), _mm256_mul_pd(x, vs));
        }
        i += 4;
    }
    while i < n {
        v[i] *= scale;
        i += 1;
    }
}

/// Computes layer forward propagation via Apple Matrix Coprocessor / BLAS Level 2 (macOS)
/// or AVX2+FMA SIMD row dot products (x86_64):
/// $\mathbf{y} = \mathbf{W} \cdot \mathbf{x} + \mathbf{b}$
/// where $\mathbf{W}$ is row-major matrix of dimension $(M \times N)$,
/// $\mathbf{x}$ is vector of length $N$,
/// and output $\mathbf{y}$ is vector of length $M$.
pub fn accelerated_layer_forward(
    weights: &[f64],
    rows: usize,
    cols: usize,
    input: &[f64],
    biases: Option<&[f64]>,
) -> Vec<f64> {
    assert_eq!(weights.len(), rows * cols, "weight matrix size must equal rows * cols");
    assert!(input.len() >= cols, "input length must be >= cols");

    let mut output = vec![0.0; rows];

    #[cfg(target_os = "macos")]
    {
        unsafe {
            // CblasRowMajor = 101, CblasNoTrans = 111
            cblas_dgemv(
                101,
                111,
                rows as i32,
                cols as i32,
                1.0,
                weights.as_ptr(),
                cols as i32,
                input.as_ptr(),
                1,
                0.0,
                output.as_mut_ptr(),
                1,
            );
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let x = &input[..cols];
        for (r, out_val) in output.iter_mut().enumerate().take(rows) {
            let row_offset = r * cols;
            *out_val = accelerated_dot_product(&weights[row_offset..row_offset + cols], x);
        }
    }

    // Apply additive biases if supplied
    if let Some(b) = biases {
        for (y, bias) in output.iter_mut().zip(b.iter()) {
            *y += bias;
        }
    }

    output
}

/// Evaluates vectorized sigmoid activation function: $\sigma(x) = \frac{1}{1 + e^{-x}}$.
pub fn accelerated_sigmoid_activation(input: &[f64]) -> Vec<f64> {
    input.iter().map(|&x| 1.0 / (1.0 + (-x).exp())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accelerated_dot_product() {
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let b = vec![2.0, 0.5, 1.0, -1.0, 3.0];
        // 1*2 + 2*0.5 + 3*1 + 4*(-1) + 5*3 = 2 + 1 + 3 - 4 + 15 = 17
        let dot = accelerated_dot_product(&a, &b);
        assert!((dot - 17.0).abs() < 1e-6);

        // Also test multi-lane AVX2 unrolled path (>8 elements + tail)
        let a_big: Vec<f64> = (1..=35).map(|x| x as f64).collect();
        let b_big: Vec<f64> = (1..=35).map(|x| (x as f64) * 0.5).collect();
        let expected: f64 = a_big.iter().zip(b_big.iter()).map(|(x, y)| x * y).sum();
        assert!((accelerated_dot_product(&a_big, &b_big) - expected).abs() < 1e-6);
    }

    #[test]
    fn test_accelerated_layer_forward() {
        // 2 rows, 3 cols:
        // [1.0, 2.0, 3.0]
        // [4.0, 5.0, 6.0]
        let weights = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let input = vec![1.0, 0.5, 2.0];
        let bias = vec![0.5, -1.0];

        // Row 0: 1*1 + 2*0.5 + 3*2 + 0.5 = 1 + 1 + 6 + 0.5 = 8.5
        // Row 1: 4*1 + 5*0.5 + 6*2 - 1.0 = 4 + 2.5 + 12 - 1.0 = 17.5
        let output = accelerated_layer_forward(&weights, 2, 3, &input, Some(&bias));
        assert_eq!(output.len(), 2);
        assert!((output[0] - 8.5).abs() < 1e-6);
        assert!((output[1] - 17.5).abs() < 1e-6);
    }

    #[test]
    fn test_accelerator_telemetry() {
        let info = accelerator_info();
        assert!(!info.is_empty());
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert!(info.contains("macOS arm64 Accelerate"));
    }
}
