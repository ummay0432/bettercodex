use super::*;

#[test]
fn setup_control_is_only_the_terminal_exact_envelope() {
    let response = "I mentioned SETUP: BLOCKED above.\n\nSETUP: READY\nCONTRACT: evaluator/workspace/contract.json\nBASELINE: evaluator/workspace/baseline.json\nBLOCKER: none\n";
    let parsed = parse_setup_envelope(response).unwrap();
    assert_eq!(parsed.verdict, SetupVerdict::Ready);
    assert_eq!(parsed.contract, "evaluator/workspace/contract.json");
    assert!(
        parse_setup_envelope("SETUP: READY\nCONTRACT: x\nBASELINE: y\nBLOCKER: none\nextra")
            .is_err()
    );
    assert!(
        parse_setup_envelope("SETUP: READY\nCONTRACT: x\nBASELINE: y\nBLOCKER: maybe").is_err()
    );
}

#[test]
fn worker_envelope_rejects_missing_or_extra_control_lines() {
    let response = "VERDICT: KEEP\nDESCRIPTION: replaced linear scan with indexed lookup\nEVIDENCE: iterations/1/worker.json\nUNVALIDATED: none";
    assert_eq!(
        parse_worker_envelope(response).unwrap().verdict,
        WorkerVerdict::Keep
    );
    assert!(
        parse_worker_envelope("VERDICT: KEEP\nDESCRIPTION: none\nEVIDENCE: x\nUNVALIDATED: none")
            .is_err()
    );
    assert!(
        parse_worker_envelope(
            "VERDICT: KEEP\nDESCRIPTION: changed it\nEVIDENCE: none\nUNVALIDATED: none"
        )
        .is_err()
    );
    assert!(parse_worker_envelope("VERDICT: KEEP\nDESCRIPTION: changed it\nEVIDENCE: x\nUNVALIDATED: none\nVERDICT: DISCARD").is_err());
}
