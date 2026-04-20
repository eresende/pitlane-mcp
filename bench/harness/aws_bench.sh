#!/usr/bin/env bash
# Spin up an AWS EC2 instance and run the real-world benchmark.
#
# Prerequisites:
#   - AWS CLI configured (aws configure or IAM role)
#   - A key pair already created in your target region
#   - pitlane-mcp binary available at a public URL or S3 path (see PITLANE_URL below)
#
# Usage:
#   bash bench/harness/aws_bench.sh [options]
#
# Options:
#   --instance-type TYPE   EC2 instance type (default: g5.xlarge)
#   --region REGION        AWS region (default: eu-west-1)
#   --key-name NAME        EC2 key pair name (required)
#   --model MODEL          Ollama model to use (default: qwen3:14b)
#   --repo REPO            Benchmark repo name under bench/repos/ (default: ripgrep)
#   --prompts FILE         Prompts JSONL file (default: prompts/ripgrep.jsonl)
#   --out DIR              Output directory on the instance (default: /home/ubuntu/results)
#   --spot                 Use spot instance (cheaper, may be interrupted)
#   --no-terminate         Don't terminate instance after run (for debugging)
#   --wait-for-quota       Poll until EC2 quota is live before launching
#   --runs N               Runs per prompt (default: 3)
#   --max-iterations N     Max agentic loop iterations per run (default: 25)
#   --help                 Show this help
#
# Example:
#   bash bench/harness/aws_bench.sh --key-name my-key --spot
#   bash bench/harness/aws_bench.sh --key-name my-key --instance-type g4dn.xlarge --model qwen3:8b

set -euo pipefail

# Clean up security group on unexpected exit
SG_ID=""
_cleanup() {
    if [[ -n "$SG_ID" ]]; then
        aws ec2 delete-security-group --group-id "$SG_ID" --region "$REGION" 2>/dev/null || true
    fi
}
trap _cleanup ERR

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
INSTANCE_TYPE="g5.xlarge"
REGION="eu-west-1"
KEY_NAME=""
MODEL="qwen3:14b"
BENCH_REPO="ripgrep"
PROMPTS_FILE="prompts/ripgrep.jsonl"
OUT_DIR="/home/ubuntu/results"
USE_SPOT=false
NO_TERMINATE=false
WAIT_FOR_QUOTA=false
RUNS=3
MAX_ITERATIONS=25

# Deep Learning Base OSS Nvidia Driver GPU AMI (Ubuntu 22.04) — eu-west-1
# Refreshed: 2026-04-10
# To find the latest for a different region:
#   aws ec2 describe-images --owners amazon \
#     --filters "Name=name,Values=Deep Learning Base OSS Nvidia Driver GPU AMI (Ubuntu 22.04)*" \
#     --query 'sort_by(Images,&CreationDate)[-1].ImageId' --region <region>
AMI_ID="ami-0a85fa8b5de3d6600"

# pitlane-mcp release tarball — fetched at runtime from the latest GitHub release
PITLANE_TARBALL_URL="https://github.com/eresende/pitlane-mcp/releases/latest/download/pitlane-mcp-linux-x86_64.tar.gz"

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --instance-type) INSTANCE_TYPE="$2"; shift 2 ;;
        --region)        REGION="$2";        shift 2 ;;
        --key-name)      KEY_NAME="$2";      shift 2 ;;
        --model)         MODEL="$2";         shift 2 ;;
        --repo)          BENCH_REPO="$2";    shift 2 ;;
        --prompts)       PROMPTS_FILE="$2";  shift 2 ;;
        --out)           OUT_DIR="$2";       shift 2 ;;
        --spot)          USE_SPOT=true;        shift ;;
        --no-terminate)  NO_TERMINATE=true;    shift ;;
        --wait-for-quota) WAIT_FOR_QUOTA=true; shift ;;
        --runs)          RUNS="$2";            shift 2 ;;
        --max-iterations) MAX_ITERATIONS="$2"; shift 2 ;;
        --help)
            sed -n '3,30p' "$0"
            exit 0
            ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

if [[ -z "$KEY_NAME" ]]; then
    echo "ERROR: --key-name is required."
    echo "  List your key pairs: aws ec2 describe-key-pairs --region $REGION --query 'KeyPairs[].KeyName'"
    exit 1
fi

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
log() { echo "[$(date '+%H:%M:%S')] $*"; }

# ---------------------------------------------------------------------------
# Optional: wait for quota to propagate before attempting launch
# ---------------------------------------------------------------------------
if $WAIT_FOR_QUOTA; then
    log "Waiting for G/VT vCPU quota to propagate in EC2 (checking every 2 minutes)..."
    while true; do
        ERR=$(aws ec2 run-instances \
            --image-id "$AMI_ID" \
            --instance-type "$INSTANCE_TYPE" \
            --key-name "$KEY_NAME" \
            --region "$REGION" \
            --dry-run \
            --count 1 2>&1 || true)
        if echo "$ERR" | grep -q "DryRunOperation"; then
            log "EC2 quota confirmed — proceeding."
            break
        elif echo "$ERR" | grep -q "VcpuLimitExceeded"; then
            log "  EC2 still enforcing old limit, retrying in 2 minutes..."
            sleep 120
        else
            log "  Unexpected response: $ERR"
            log "  Retrying in 2 minutes..."
            sleep 120
        fi
    done
fi

# ---------------------------------------------------------------------------
# Security group — allow SSH only
# ---------------------------------------------------------------------------
SG_NAME="pitlane-bench-sg-$$"
log "Creating security group $SG_NAME ..."
SG_ID=$(aws ec2 create-security-group \
    --group-name "$SG_NAME" \
    --description "pitlane-mcp benchmark (temporary)" \
    --region "$REGION" \
    --query 'GroupId' --output text)

aws ec2 authorize-security-group-ingress \
    --group-id "$SG_ID" \
    --protocol tcp --port 22 --cidr 0.0.0.0/0 \
    --region "$REGION" > /dev/null

log "Security group: $SG_ID"

# ---------------------------------------------------------------------------
# User-data script — runs on the instance at boot
# ---------------------------------------------------------------------------
USER_DATA=$(cat <<USERDATA
#!/bin/bash
set -euo pipefail
exec > /var/log/bench-init.log 2>&1

# user-data runs as root without a login shell — set HOME explicitly
export HOME=/root

echo "=== Installing dependencies ==="
apt-get update -q
apt-get install -y -q git python3-pip python3-venv curl

echo "=== Installing Ollama ==="
curl -fsSL https://ollama.com/install.sh | sh
# systemd is not available in user-data — start ollama directly as a background daemon
OLLAMA_HOST=127.0.0.1:11434 HOME=/root ollama serve &>/var/log/ollama.log &
sleep 8  # wait for the API to be ready

echo "=== Pulling model: ${MODEL} ==="
HOME=/root ollama pull ${MODEL}

echo "=== Installing pitlane-mcp ==="
curl -fsSL "${PITLANE_TARBALL_URL}" -o /tmp/pitlane-mcp.tar.gz
tar -xzf /tmp/pitlane-mcp.tar.gz -C /usr/local/bin/
chmod +x /usr/local/bin/pitlane-mcp

echo "=== Cloning pitlane-mcp repo ==="
git clone --branch model-benchmarks https://github.com/eresende/pitlane-mcp.git /home/ubuntu/pitlane-mcp
chown -R ubuntu:ubuntu /home/ubuntu/pitlane-mcp

echo "=== Setting up Python environment ==="
cd /home/ubuntu/pitlane-mcp
python3 -m venv .venv
.venv/bin/pip install -q hypothesis pytest

echo "=== Cloning benchmark repos ==="
bash bench/setup.sh 2>&1 | tail -5

echo "=== Running benchmark ==="
mkdir -p "${OUT_DIR}"
PYTHONPATH=/home/ubuntu/pitlane-mcp .venv/bin/python -m bench.harness.bench_runner \
    --repo bench/repos/${BENCH_REPO} \
    --prompts bench/harness/${PROMPTS_FILE} \
    --model ${MODEL} \
    --out ${OUT_DIR} \
    --runs ${RUNS} \
    --max-iterations ${MAX_ITERATIONS} \
    2>&1 | tee /var/log/bench-run.log

echo "=== Done ==="
touch /tmp/bench-complete
USERDATA
)

# ---------------------------------------------------------------------------
# Launch instance
# ---------------------------------------------------------------------------
LAUNCH_ARGS=(
    --image-id "$AMI_ID"
    --instance-type "$INSTANCE_TYPE"
    --key-name "$KEY_NAME"
    --security-group-ids "$SG_ID"
    --region "$REGION"
    --user-data "$USER_DATA"
    --block-device-mappings '[{"DeviceName":"/dev/sda1","Ebs":{"VolumeSize":100,"VolumeType":"gp3"}}]'
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=pitlane-bench},{Key=Purpose,Value=benchmark}]"
    --query 'Instances[0].InstanceId'
    --output text
)

if $USE_SPOT; then
    log "Requesting spot instance ($INSTANCE_TYPE) ..."
    INSTANCE_ID=$(aws ec2 run-instances "${LAUNCH_ARGS[@]}" \
        --instance-market-options '{"MarketType":"spot","SpotOptions":{"SpotInstanceType":"one-time"}}')
else
    log "Launching on-demand instance ($INSTANCE_TYPE) ..."
    INSTANCE_ID=$(aws ec2 run-instances "${LAUNCH_ARGS[@]}")
fi

log "Instance ID: $INSTANCE_ID"

# ---------------------------------------------------------------------------
# Wait for instance to be running and get public IP
# ---------------------------------------------------------------------------
log "Waiting for instance to start ..."
aws ec2 wait instance-running --instance-ids "$INSTANCE_ID" --region "$REGION"

PUBLIC_IP=$(aws ec2 describe-instances \
    --instance-ids "$INSTANCE_ID" \
    --region "$REGION" \
    --query 'Reservations[0].Instances[0].PublicIpAddress' \
    --output text)

log "Instance running at $PUBLIC_IP"
log ""
log "SSH access:"
log "  ssh ubuntu@${PUBLIC_IP}"
log ""
log "Watch progress:"
log "  ssh ubuntu@${PUBLIC_IP} 'tail -f /var/log/bench-run.log'"
log ""
log "Check if complete:"
log "  ssh ubuntu@${PUBLIC_IP} 'ls /tmp/bench-complete 2>/dev/null && echo DONE || echo RUNNING'"

# ---------------------------------------------------------------------------
# Wait for benchmark to complete (poll via SSH)
# ---------------------------------------------------------------------------
log ""
log "Waiting for benchmark to complete (this will take 30-90 minutes) ..."
log "Press Ctrl+C to stop waiting — the instance will keep running."

SSH_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=10 -o BatchMode=yes"
SSH_KEY=""

# Wait for SSH to become available
for i in $(seq 1 30); do
    if ssh $SSH_OPTS "ubuntu@${PUBLIC_IP}" 'echo ok' &>/dev/null; then
        break
    fi
    sleep 10
done

# Poll for completion
while true; do
    if ssh $SSH_OPTS "ubuntu@${PUBLIC_IP}" 'test -f /tmp/bench-complete' 2>/dev/null; then
        log "Benchmark complete."
        break
    fi
    # Show last log line
    LAST=$(ssh $SSH_OPTS "ubuntu@${PUBLIC_IP}" \
        'tail -1 /var/log/bench-run.log 2>/dev/null || echo "initializing..."' 2>/dev/null || echo "waiting for SSH...")
    log "  $LAST"
    sleep 30
done

# ---------------------------------------------------------------------------
# Download results
# ---------------------------------------------------------------------------
LOCAL_OUT="result/aws-$(date '+%Y%m%d-%H%M%S')"
mkdir -p "$LOCAL_OUT"
log "Downloading results to $LOCAL_OUT ..."
scp $SSH_OPTS -r "ubuntu@${PUBLIC_IP}:${OUT_DIR}/" "$LOCAL_OUT/"
log "Results saved to $LOCAL_OUT"

# ---------------------------------------------------------------------------
# Terminate instance
# ---------------------------------------------------------------------------
if $NO_TERMINATE; then
    log "Skipping termination (--no-terminate). Don't forget to terminate manually:"
    log "  aws ec2 terminate-instances --instance-ids $INSTANCE_ID --region $REGION"
else
    log "Terminating instance $INSTANCE_ID ..."
    aws ec2 terminate-instances --instance-ids "$INSTANCE_ID" --region "$REGION" > /dev/null
    log "Waiting for instance to terminate before cleaning up security group..."
    aws ec2 wait instance-terminated --instance-ids "$INSTANCE_ID" --region "$REGION"
    aws ec2 delete-security-group --group-id "$SG_ID" --region "$REGION" > /dev/null || \
        log "Note: security group $SG_ID can be deleted manually once the instance is gone."
    log "Instance terminated."
fi

log ""
log "Done. Results are in $LOCAL_OUT/"
