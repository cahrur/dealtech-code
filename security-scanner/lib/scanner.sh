#!/bin/bash
# Scanner core logic - runs Nuclei with appropriate templates

run_scan() {
    local target="$1"
    local mode="$2"
    local severity="$3"
    local auth_args="$4"
    local output_file="$5"

    local nuclei_args="-u ${target} -severity ${severity} -silent -no-color"
    
    # Add auth if provided
    if [ -n "$auth_args" ]; then
        nuclei_args="${nuclei_args} ${auth_args}"
    fi

    # JSON output for parsing
    nuclei_args="${nuclei_args} -jsonl -o ${output_file}.jsonl"

    case "$mode" in
        quick)
            # Top critical templates - fast scan
            nuclei_args="${nuclei_args} -tags cve,rce,sqli,xss,lfi,ssrf,redirect,exposure"
            nuclei_args="${nuclei_args} -rate-limit 100"
            nuclei_args="${nuclei_args} -timeout 10"
            nuclei_args="${nuclei_args} -retries 1"
            ;;
        full)
            # All templates - comprehensive
            nuclei_args="${nuclei_args} -rate-limit 50"
            nuclei_args="${nuclei_args} -timeout 15"
            nuclei_args="${nuclei_args} -retries 2"
            ;;
        recon)
            # Info gathering only
            nuclei_args="${nuclei_args} -tags tech,dns,waf -severity info,low"
            nuclei_args="${nuclei_args} -rate-limit 150"
            ;;
        cves)
            # Known CVEs
            nuclei_args="${nuclei_args} -tags cve"
            nuclei_args="${nuclei_args} -rate-limit 75"
            ;;
        misconfig)
            # Misconfigurations
            nuclei_args="${nuclei_args} -tags misconfig,misconfiguration"
            nuclei_args="${nuclei_args} -rate-limit 100"
            ;;
        exposure)
            # Exposed files, panels, sensitive data
            nuclei_args="${nuclei_args} -tags exposure,panel,token,config"
            nuclei_args="${nuclei_args} -rate-limit 100"
            ;;
    esac

    # Run nuclei
    echo "Running: nuclei (mode: ${mode})"
    echo "Templates: $(get_template_info "$mode")"
    echo ""

    eval nuclei ${nuclei_args} 2>/dev/null

    # Also save human-readable output
    if [ -f "${output_file}.jsonl" ]; then
        local count=$(wc -l < "${output_file}.jsonl")
        echo ""
        echo "Found ${count} potential issue(s)"
        return 0
    else
        echo "No issues found or scan produced no output"
        touch "${output_file}.jsonl"
        return 0
    fi
}

get_template_info() {
    local mode="$1"
    case "$mode" in
        quick) echo "CVE, RCE, SQLi, XSS, LFI, SSRF, Redirect, Exposure" ;;
        full) echo "All available templates" ;;
        recon) echo "Technology detection, DNS, WAF" ;;
        cves) echo "Known CVEs" ;;
        misconfig) echo "Misconfigurations" ;;
        exposure) echo "Exposed files, panels, tokens" ;;
    esac
}
