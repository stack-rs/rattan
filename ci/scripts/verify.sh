#!/usr/bin/env bash
set -e

IPERF_ITERS=10
PING_ITERS=100
WORKDIR=$(
	cd "$(dirname "$0")"
	pwd
)
RESULT_FILE=verify.html
PING_COL=("mean" "pstdev" "min" "q1" "median" "q3" "max")
PING_LOSS_COL=("Loss" "Accept")
IPERF_COL=("mean" "pstdev" "min" "q1" "median" "q3" "max")
IPERF_RETR_COL=("sum")

# Enter working dir
original_dir=$(pwd)
script_dir="$(dirname "$(readlink -f "$0")")"
cd $script_dir

# Creat work dir
mkdir -p "cache"
cd "cache"
echo "" >$RESULT_FILE

# Calculate color from value
# $1 is value
# $2 is max
calc_color() {
	value=${1#-}
	max=$2
	middle=$(echo "scale=4; $max / 2" | bc)
	# echo "value: $value, max: $max, middle: $middle"
	if [[ $(echo "$value > $max" | bc) -eq 1 ]]; then
		r=255
		g=0
		b=0
	elif [[ $(echo "$value > $middle" | bc) -eq 1 ]]; then
		r=255
		if [ $(echo "$middle == 0" | bc) -eq 1 ]; then
			g=0
		else
			g=$(echo "scale=4; 255 * (1 - ($value - $middle) / $middle)" | bc)
		fi
		b=0
	else
		if [ $(echo "$middle == 0" | bc) -eq 1 ]; then
			r=0
		else
			r=$(echo "scale=4; 255 * ($value / $middle)" | bc)
		fi
		g=255
		b=0
	fi
	r=$(printf "%.0f" $r)
	g=$(printf "%.0f" $g)
	b=$(printf "%.0f" $b)
}

calc_ping_stats() {
	delay=$(echo "$1 * 2" | bc)
	loss=$2
	suffix=$3

	values=($(cat "rattan_${suffix}_ping.log" | grep "time=" | awk '{print $7}' | cut -c 6- | datamash -s mean 1 pstdev 1 min 1 q1 1 median 1 q3 1 max 1))
	for ((i = 0; i < ${#values[@]}; i++)); do
		case ${PING_COL[$i]} in
		"mean" | "median")
			calc_color $(echo "${values[$i]} - $delay" | bc) $(echo "$delay * 0.1 + 1" | bc)
			echo "<td style=\"background-color:rgba(${r},${g},${b},0.6)\">${values[$i]}</td>" >>$RESULT_FILE
			;;
		"pstdev")
			calc_color ${values[$i]} $(echo "$delay * 0.05 + 0.5" | bc)
			echo "<td style=\"background-color:rgba(${r},${g},${b},0.6)\">${values[$i]}</td>" >>$RESULT_FILE
			;;
		*)
			echo "<td>${values[$i]}</td>" >>$RESULT_FILE
			;;
		esac
	done

	values=($(cat "rattan_${suffix}_ping.log" | grep "packet loss" | awk '{print $6, $4"/"$1 }'))
	for ((i = 0; i < ${#values[@]}; i++)); do
		case ${PING_LOSS_COL[$i]} in
		"Loss")
			calc_color $(echo "scale=4; ${values[$i]%\%} / 100 - $loss" | bc) $(echo "scale=4; $loss * 0.1" | bc)
			echo "<td style=\"background-color:rgba(${r},${g},${b},0.6)\">${values[$i]}</td>" >>$RESULT_FILE
			;;
		*)
			echo "<td>${values[$i]}</td>" >>$RESULT_FILE
			;;
		esac
	done
}

ping_verify() {
	delay=$1
	loss=$2
	suffix="delay${delay}_loss${loss}"

	echo "Ping Test: Loss $loss; Delay $delay ms"
	RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' \
		cargo run --release -q -p rattan -- \
		link \
		--uplink-delay ${delay}ms \
		--downlink-delay ${delay}ms \
		--downlink-loss $loss \
		-- $WORKDIR/ping.sh $PING_ITERS '$RATTAN_BASE' \
		>"rattan_${suffix}_ping.log" 2>&1
	calc_ping_stats $delay $loss $suffix
}

calc_iperf_stats() {
	bandwidth=$1
	loss=$2
	suffix=$3

	values=($(cat "rattan_$suffix.log" | grep "Mbits/sec" | awk '{print $7}' | head -n $IPERF_ITERS | datamash -s mean 1 pstdev 1 min 1 q1 1 median 1 q3 1 max 1))
	for ((i = 0; i < ${#values[@]}; i++)); do
		case ${IPERF_COL[$i]} in
		"mean" | "median")
			calc_color $(echo "${values[$i]} - $bandwidth" | bc) $(echo "$bandwidth * 0.2" | bc)
			echo "<td style=\"background-color:rgba(${r},${g},${b},0.6)\">${values[$i]}</td>" >>$RESULT_FILE
			;;
		"pstdev")
			calc_color ${values[$i]} $(echo "$bandwidth * 0.1" | bc)
			echo "<td style=\"background-color:rgba(${r},${g},${b},0.6)\">${values[$i]}</td>" >>$RESULT_FILE
			;;
		*)
			echo "<td>${values[$i]}</td>" >>$RESULT_FILE
			;;
		esac
	done

	values=($(cat "rattan_$suffix.log" | grep "sender" | awk '{print $9}'))
	for ((i = 0; i < ${#values[@]}; i++)); do
		echo "<td>${values[$i]}</td>" >>$RESULT_FILE
	done
}

iperf_verify() {
	delay=$1
	loss=$2
	bw_mul=$3
	cc=$4
	bandwidth=$(expr 12 \* $bw_mul)
	suffix="delay${delay}_loss${loss}_bw${bandwidth}_${cc}"

	echo "iPerf Test: Loss $loss; Bandwidth $bandwidth Mbps; Delay $delay ms"
	bandwidth=$(expr 12 \* $bw_mul \* 1000000)
	RUST_LOG=info CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' \
		cargo run --release -q -p rattan -- \
		link \
		--uplink-delay ${delay}ms \
		--downlink-delay ${delay}ms \
		--downlink-loss $loss \
		--uplink-bandwidth "$bandwidth"Mbps \
		--downlink-bandwidth "$bandwidth"Mbps \
		-- $WORKDIR/iperf3.sh $IPERF_ITERS '$RATTAN_BASE' $cc \
		>"rattan_$suffix.log" 2>&1
	calc_iperf_stats $(expr 12 \* $bw_mul) $loss $suffix
}

# ---------- Ping Test ----------
echo '<table border="2" bordercolor="black" cellspacing="0" cellpadding="5">' >>$RESULT_FILE
echo "<caption><h3>PING TEST</h3></caption>" >>$RESULT_FILE

echo "<tr align="center">" >>$RESULT_FILE
echo "<td colspan="2"><b>Environment</b></td>" >>$RESULT_FILE
echo "<td colspan="${#IPERF_COL[@]}"><b>Ping RTT (ms)</b></td>" >>$RESULT_FILE
echo "<td colspan="${#IPERF_RETR_COL[@]}"><b>Loss</b></td>" >>$RESULT_FILE
echo "</tr>" >>$RESULT_FILE

echo "<tr align="center">" >>$RESULT_FILE
echo "<td><b>LOSS</b></td>" >>$RESULT_FILE
echo "<td><b>RTT</b></td>" >>$RESULT_FILE
for ((i = 0; i < ${#PING_COL[@]}; i++)); do
	echo "<td>${PING_COL[$i]}</td>" >>$RESULT_FILE
done
for ((i = 0; i < ${#PING_LOSS_COL[@]}; i++)); do
	echo "<td>${PING_LOSS_COL[$i]}</td>" >>$RESULT_FILE
done
echo "</tr>" >>$RESULT_FILE

for loss in 0 0.1 0.5; do
	for delay in 0 1 10 20 50 500; do
		echo "<tr align="center">" >>$RESULT_FILE
		echo "<td>$loss</td>" >>$RESULT_FILE
		echo "<td>$(expr 2 \* $delay)ms</td>" >>$RESULT_FILE
		ping_verify $delay $loss
		echo "</tr>" >>$RESULT_FILE
	done
done
echo "</table>" >>$RESULT_FILE

# ---------- iPerf Test ----------
echo ""
echo "Start iperf3 server"
iperf3 -s >iperf3_server.log 2>&1 &
server_pid=$!
sleep 1

echo "<br></br>" >>$RESULT_FILE
echo '<table border="2" bordercolor="black" cellspacing="0" cellpadding="5">' >>$RESULT_FILE
echo "<caption><h3>IPERF TEST</h3></caption>" >>$RESULT_FILE

echo "<tr align="center">" >>$RESULT_FILE
echo "<td colspan="4"><b>Environment</b></td>" >>$RESULT_FILE
echo "<td colspan="${#IPERF_COL[@]}"><b>Throughout (Mbps)</b></td>" >>$RESULT_FILE
echo "<td colspan="${#IPERF_RETR_COL[@]}"><b>Retransmission</b></td>" >>$RESULT_FILE
echo "</tr>" >>$RESULT_FILE

echo "<tr align="center">" >>$RESULT_FILE
echo "<td><b>LOSS</b></td>" >>$RESULT_FILE
echo "<td><b>BW</b></td>" >>$RESULT_FILE
echo "<td><b>RTT</b></td>" >>$RESULT_FILE
echo "<td><b>CC</b></td>" >>$RESULT_FILE
for ((i = 0; i < ${#IPERF_COL[@]}; i++)); do
	echo "<td>${IPERF_COL[$i]}</td>" >>$RESULT_FILE
done
for ((i = 0; i < ${#IPERF_RETR_COL[@]}; i++)); do
	echo "<td>${IPERF_RETR_COL[$i]}</td>" >>$RESULT_FILE
done
echo "</tr>" >>$RESULT_FILE

for bw_mul in 50 100; do
	for delay in 0 1 5 10; do
		echo "<tr align="center">" >>$RESULT_FILE
		echo "<td>0</td>" >>$RESULT_FILE
		echo "<td>$(expr 12 \* $bw_mul)Mbps</td>" >>$RESULT_FILE
		echo "<td>$(expr 2 \* $delay)ms</td>" >>$RESULT_FILE
		echo "<td>cubic</td>" >>$RESULT_FILE
		iperf_verify $delay 0 $bw_mul cubic
		echo "</tr>" >>$RESULT_FILE
	done
done

echo "<tr align="center">" >>$RESULT_FILE
echo "</tr>" >>$RESULT_FILE

for loss in 0 0.01; do
	for bw_mul in 1 2 5 10; do
		for delay in 0 10 50 100; do
			echo "<tr align="center">" >>$RESULT_FILE
			echo "<td>$loss</td>" >>$RESULT_FILE
			echo "<td>$(expr 12 \* $bw_mul)Mbps</td>" >>$RESULT_FILE
			echo "<td>$(expr 2 \* $delay)ms</td>" >>$RESULT_FILE
			if [ $loss == 0 ]; then
				echo "<td>cubic</td>" >>$RESULT_FILE
				iperf_verify $delay $loss $bw_mul cubic
			else
				set +e
				sysctl net.ipv4.tcp_available_congestion_control | grep bbr
				if [ $? -eq 0 ]; then
					echo "<td>bbr</td>" >>$RESULT_FILE
					iperf_verify $delay $loss $bw_mul bbr
				else
					echo "Warn: bbr not available"
					echo "<td>cubic</td>" >>$RESULT_FILE
					iperf_verify $delay $loss $bw_mul cubic
				fi
				set -e
			fi
			echo "</tr>" >>$RESULT_FILE
		done
	done
done

echo "kill iperf3 server"
kill -9 $server_pid

# Back to original dir
cd $original_dir
