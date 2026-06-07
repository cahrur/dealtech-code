#!/bin/bash
# Reporter - formats scan results

generate_report() {
    local output_file="$1"
    local format="$2"
    local target="$3"
    local mode="$4"

    local jsonl_file="${output_file}.jsonl"
    local total=$(wc -l < "$jsonl_file" 2>/dev/null || echo "0")

    if [ "$total" -eq 0 ]; then
        echo -e "${GREEN}✅ No vulnerabilities found!${NC}"
        echo ""
        echo "Note: This doesn't mean the target is 100% secure."
        echo "Automated scanners catch ~30-40% of issues."
        echo "Consider manual testing for business logic flaws."
        return 0
    fi

    # Count by severity
    local critical=$(grep -c '"critical"' "$jsonl_file" 2>/dev/null || echo "0")
    local high=$(grep -c '"high"' "$jsonl_file" 2>/dev/null || echo "0")
    local medium=$(grep -c '"medium"' "$jsonl_file" 2>/dev/null || echo "0")
    local low=$(grep -c '"low"' "$jsonl_file" 2>/dev/null || echo "0")
    local info=$(grep -c '"info"' "$jsonl_file" 2>/dev/null || echo "0")

    case "$format" in
        text) report_text "$jsonl_file" "$target" "$mode" "$total" "$critical" "$high" "$medium" "$low" "$info" ;;
        json) report_json "$jsonl_file" "$target" "$mode" ;;
        markdown) report_markdown "$jsonl_file" "$target" "$mode" "$total" "$critical" "$high" "$medium" "$low" "$info" ;;
    esac
}

report_text() {
    local jsonl_file="$1" target="$2" mode="$3" total="$4"
    local critical="$5" high="$6" medium="$7" low="$8" info="$9"

    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo -e "📊 ${BLUE}SCAN RESULTS${NC}"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "Target: ${target}"
    echo "Mode:   ${mode}"
    echo "Total:  ${total} finding(s)"
    echo ""
    echo "Severity Breakdown:"
    [ "$critical" -gt 0 ] && echo -e "  🔴 Critical: ${RED}${critical}${NC}"
    [ "$high" -gt 0 ] && echo -e "  🟠 High:     ${RED}${high}${NC}"
    [ "$medium" -gt 0 ] && echo -e "  🟡 Medium:   ${YELLOW}${medium}${NC}"
    [ "$low" -gt 0 ] && echo -e "  🔵 Low:      ${low}"
    [ "$info" -gt 0 ] && echo -e "  ⚪ Info:     ${info}"
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "FINDINGS:"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""

    # Parse and display each finding
    while IFS= read -r line; do
        local name=$(echo "$line" | jq -r '.info.name // "Unknown"')
        local sev=$(echo "$line" | jq -r '.info.severity // "unknown"')
        local matched=$(echo "$line" | jq -r '.matched-at // .host // "N/A"')
        local template_id=$(echo "$line" | jq -r '.["template-id"] // "unknown"')
        local desc=$(echo "$line" | jq -r '.info.description // ""' | head -2)

        local sev_icon=""
        case "$sev" in
            critical) sev_icon="🔴" ;;
            high) sev_icon="🟠" ;;
            medium) sev_icon="🟡" ;;
            low) sev_icon="🔵" ;;
            *) sev_icon="⚪" ;;
        esac

        echo -e "${sev_icon} [${sev^^}] ${name}"
        echo "   Template: ${template_id}"
        echo "   URL: ${matched}"
        [ -n "$desc" ] && echo "   Info: ${desc}"
        echo ""
    done < "$jsonl_file"

    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "Full results: ${jsonl_file}"
}

report_json() {
    local jsonl_file="$1" target="$2" mode="$3"
    # Output as proper JSON array
    echo "["
    local first=true
    while IFS= read -r line; do
        if [ "$first" = true ]; then
            first=false
        else
            echo ","
        fi
        echo "$line"
    done < "$jsonl_file"
    echo "]"
}

report_markdown() {
    local jsonl_file="$1" target="$2" mode="$3" total="$4"
    local critical="$5" high="$6" medium="$7" low="$8" info="$9"

    local md_file="${jsonl_file%.jsonl}.md"

    {
        echo "# Security Scan Report"
        echo ""
        echo "- **Target:** ${target}"
        echo "- **Mode:** ${mode}"
        echo "- **Date:** $(date '+%Y-%m-%d %H:%M:%S')"
        echo "- **Total Findings:** ${total}"
        echo ""
        echo "## Summary"
        echo ""
        echo "| Severity | Count |"
        echo "|----------|-------|"
        echo "| 🔴 Critical | ${critical} |"
        echo "| 🟠 High | ${high} |"
        echo "| 🟡 Medium | ${medium} |"
        echo "| 🔵 Low | ${low} |"
        echo "| ⚪ Info | ${info} |"
        echo ""
        echo "## Findings"
        echo ""

        while IFS= read -r line; do
            local name=$(echo "$line" | jq -r '.info.name // "Unknown"')
            local sev=$(echo "$line" | jq -r '.info.severity // "unknown"')
            local matched=$(echo "$line" | jq -r '.matched-at // .host // "N/A"')
            local template_id=$(echo "$line" | jq -r '.["template-id"] // "unknown"')
            local desc=$(echo "$line" | jq -r '.info.description // "No description"')
            local ref=$(echo "$line" | jq -r '.info.reference // [] | join(", ")' 2>/dev/null)

            echo "### [${sev^^}] ${name}"
            echo ""
            echo "- **Template:** \`${template_id}\`"
            echo "- **URL:** \`${matched}\`"
            echo "- **Description:** ${desc}"
            [ -n "$ref" ] && [ "$ref" != "null" ] && echo "- **References:** ${ref}"
            echo ""
        done < "$jsonl_file"

        echo "---"
        echo "*Generated by Security Scanner (Nuclei-based)*"
    } > "$md_file"

    echo "📄 Markdown report saved: ${md_file}"
}
