;;! target = "x86_64"

(module
    (func (result i64)
	(i64.const 1)
	(i64.const 0)
	(i64.rem_u)
    )
)
;;    0:	 55                   	push	rbp
;;    1:	 4889e5               	mov	rbp, rsp
;;    4:	 48c7c100000000       	mov	rcx, 0
;;    b:	 48c7c001000000       	mov	rax, 1
;;   12:	 4883f900             	cmp	rcx, 0
;;   16:	 0f8502000000         	jne	0x1e
;;   1c:	 0f0b                 	ud2	
;;   1e:	 4831d2               	xor	rdx, rdx
;;   21:	 48f7f1               	div	rcx
;;   24:	 4889d0               	mov	rax, rdx
;;   27:	 5d                   	pop	rbp
;;   28:	 c3                   	ret	
