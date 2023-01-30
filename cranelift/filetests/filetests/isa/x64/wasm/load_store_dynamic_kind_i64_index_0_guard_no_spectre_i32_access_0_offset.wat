;;! target = "x86_64"
;;!
;;! settings = ['enable_heap_access_spectre_mitigation=false']
;;!
;;! compile = true
;;!
;;! [globals.vmctx]
;;! type = "i64"
;;! vmctx = true
;;!
;;! [globals.heap_base]
;;! type = "i64"
;;! load = { base = "vmctx", offset = 0, readonly = true }
;;!
;;! [globals.heap_bound]
;;! type = "i64"
;;! load = { base = "vmctx", offset = 8, readonly = true }
;;!
;;! [[heaps]]
;;! base = "heap_base"
;;! min_size = 0x10000
;;! offset_guard_size = 0
;;! index_type = "i64"
;;! style = { kind = "dynamic", bound = "heap_bound" }

;; !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
;; !!! GENERATED BY 'make-load-store-tests.sh' DO NOT EDIT !!!
;; !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!

(module
  (memory i64 1)

  (func (export "do_store") (param i64 i32)
    local.get 0
    local.get 1
    i32.store offset=0)

  (func (export "do_load") (param i64) (result i32)
    local.get 0
    i32.load offset=0))

;; function u0:0:
;;   pushq   %rbp
;;   unwind PushFrameRegs { offset_upward_to_caller_sp: 16 }
;;   movq    %rsp, %rbp
;;   unwind DefineNewFrame { offset_upward_to_caller_sp: 16, offset_downward_to_clobbers: 0 }
;; block0:
;;   movabsq $-4, %r9
;;   addq    %r9, 8(%rdx), %r9
;;   cmpq    %r9, %rdi
;;   jnbe    label1; j label2
;; block2:
;;   movq    0(%rdx), %rax
;;   movl    %esi, 0(%rax,%rdi,1)
;;   jmp     label3
;; block3:
;;   movq    %rbp, %rsp
;;   popq    %rbp
;;   ret
;; block1:
;;   ud2 heap_oob
;;
;; function u0:1:
;;   pushq   %rbp
;;   unwind PushFrameRegs { offset_upward_to_caller_sp: 16 }
;;   movq    %rsp, %rbp
;;   unwind DefineNewFrame { offset_upward_to_caller_sp: 16, offset_downward_to_clobbers: 0 }
;; block0:
;;   movabsq $-4, %r9
;;   addq    %r9, 8(%rsi), %r9
;;   cmpq    %r9, %rdi
;;   jnbe    label1; j label2
;; block2:
;;   movq    0(%rsi), %rsi
;;   movl    0(%rsi,%rdi,1), %eax
;;   jmp     label3
;; block3:
;;   movq    %rbp, %rsp
;;   popq    %rbp
;;   ret
;; block1:
;;   ud2 heap_oob
