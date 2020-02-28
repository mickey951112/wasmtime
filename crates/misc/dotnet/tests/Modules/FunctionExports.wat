(module
  (func $no_params_no_results)
  (func $one_i32_param_no_results (param i32))
  (func $one_i64_param_no_results (param i64))
  (func $one_f32_param_no_results (param f32))
  (func $one_f64_param_no_results (param f64))
  (func $one_param_of_each_type (param i32 i64 f32 f64))
  (func $no_params_one_i32_result (result i32) i32.const 0)
  (func $no_params_one_i64_result (result i64) i64.const 0)
  (func $no_params_one_f32_result (result f32) f32.const 0)
  (func $no_params_one_f64_result (result f64) f64.const 0)
  (func $one_result_of_each_type (result i32 i64 f32 f64) i32.const 0 i64.const 0 f32.const 0 f64.const 0)
  (func $one_param_and_result_of_each_type (param i32 i64 f32 f64) (result i32 i64 f32 f64) i32.const 0 i64.const 0 f32.const 0 f64.const 0)
  (export "no_params_no_results" (func $no_params_no_results))
  (export "one_i32_param_no_results" (func $one_i32_param_no_results))
  (export "one_i64_param_no_results" (func $one_i64_param_no_results))
  (export "one_f32_param_no_results" (func $one_f32_param_no_results))
  (export "one_f64_param_no_results" (func $one_f64_param_no_results))
  (export "one_param_of_each_type" (func $one_param_of_each_type))
  (export "no_params_one_i32_result" (func $no_params_one_i32_result))
  (export "no_params_one_i64_result" (func $no_params_one_i64_result))
  (export "no_params_one_f32_result" (func $no_params_one_f32_result))
  (export "no_params_one_f64_result" (func $no_params_one_f64_result))
  (export "one_result_of_each_type" (func $one_result_of_each_type))
  (export "one_param_and_result_of_each_type" (func $one_param_and_result_of_each_type))
)
