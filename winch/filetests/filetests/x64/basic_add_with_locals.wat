;;! target = "x86_64"

(module
  (export "main" (func $main))

  (func $main (result i32)
        (local $foo i32)  
        (local $bar i32)
        (i32.const 10)
        (local.set $foo)
        (i32.const 20)
        (local.set $bar)

        (local.get $foo)
        (local.get $bar)
        i32.add)
)
;;    0:	 55                   	push	rbp
;;    1:	 4889e5               	mov	rbp, rsp
;;    4:	 4883ec08             	sub	rsp, 8
;;    8:	 48c7042400000000     	mov	qword ptr [rsp], 0
;;   10:	 b80a000000           	mov	eax, 0xa
;;   15:	 89442404             	mov	dword ptr [rsp + 4], eax
;;   19:	 b814000000           	mov	eax, 0x14
;;   1e:	 890424               	mov	dword ptr [rsp], eax
;;   21:	 8b0424               	mov	eax, dword ptr [rsp]
;;   24:	 8b4c2404             	mov	ecx, dword ptr [rsp + 4]
;;   28:	 01c1                 	add	ecx, eax
;;   2a:	 4889c8               	mov	rax, rcx
;;   2d:	 4883c408             	add	rsp, 8
;;   31:	 5d                   	pop	rbp
;;   32:	 c3                   	ret	
