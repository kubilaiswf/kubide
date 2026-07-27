# kubide — backdrop maliyeti ölçümü
#
# Kullanım:
#   .\spikes\olc.ps1                 # 15 saniye örnekler
#   .\spikes\olc.ps1 -Saniye 30
#
# Yöntem: DWM ve spike sürecinin CPU + GPU kullanımını örnekler.
# Anlamlı olan tek sayı FARK: aynı ölçümü backdrop kapalıyken (spike'ta 1)
# ve Acrylic'teyken (spike'ta 3) yapıp karşılaştır.
#
# Ölçüm öncesi oyun/tarayıcı gibi GPU yiyen her şeyi kapat, yoksa
# DWM'in payı onların gürültüsünde kaybolur.

param([int]$Saniye = 15)

$cores = (Get-CimInstance Win32_ComputerSystem).NumberOfLogicalProcessors

function Get-GpuPercent {
    param([int[]]$Pids)
    $s = Get-Counter '\GPU Engine(*)\Utilization Percentage' -ErrorAction SilentlyContinue
    if (-not $s) { return @{} }
    $out = @{}
    foreach ($p in $Pids) { $out[$p] = 0.0 }
    foreach ($sample in $s.CounterSamples) {
        if ($sample.InstanceName -match 'pid_(\d+)_') {
            $sp = [int]$Matches[1]
            if ($out.ContainsKey($sp)) { $out[$sp] += $sample.CookedValue }
        }
    }
    return $out
}

$dwm = Get-Process dwm -ErrorAction SilentlyContinue | Select-Object -First 1
$spike = Get-Process spike-mica-window -ErrorAction SilentlyContinue | Select-Object -First 1

if (-not $dwm) { Write-Output "dwm.exe bulunamadi"; exit 1 }
if (-not $spike) {
    Write-Output "!! spike-mica-window calismiyor. Once onu baslat."
    Write-Output "   .\spikes\01-mica-window\target\release\spike-mica-window.exe"
    exit 1
}

Write-Output "olculuyor: $Saniye saniye  (dwm pid=$($dwm.Id), spike pid=$($spike.Id), $cores cekirdek)"
Write-Output "bu sirada spike penceresiyle etkilesme - bosta olcuyoruz"
Write-Output ""

$pids = @($dwm.Id, $spike.Id)
$dwmCpu0 = $dwm.TotalProcessorTime
$spikeCpu0 = $spike.TotalProcessorTime
$t0 = Get-Date

$gpuDwm = @(); $gpuSpike = @()
for ($i = 0; $i -lt $Saniye; $i++) {
    Start-Sleep -Milliseconds 1000
    $g = Get-GpuPercent -Pids $pids
    $gpuDwm += $g[$dwm.Id]
    $gpuSpike += $g[$spike.Id]
}

$dwm.Refresh(); $spike.Refresh()
$elapsed = ((Get-Date) - $t0).TotalSeconds
$dwmCpu = ($dwm.TotalProcessorTime - $dwmCpu0).TotalSeconds / $elapsed / $cores * 100
$spikeCpu = ($spike.TotalProcessorTime - $spikeCpu0).TotalSeconds / $elapsed / $cores * 100

function Avg($a) { if ($a.Count) { [math]::Round((($a | Measure-Object -Average).Average), 2) } else { 0 } }

Write-Output "                 CPU %    GPU %    Bellek MB"
Write-Output ("dwm.exe        {0,6:N2}  {1,6:N2}   {2,8:N1}" -f $dwmCpu, (Avg $gpuDwm), ($dwm.WorkingSet64 / 1MB))
Write-Output ("spike          {0,6:N2}  {1,6:N2}   {2,8:N1}" -f $spikeCpu, (Avg $gpuSpike), ($spike.WorkingSet64 / 1MB))
Write-Output ("TOPLAM         {0,6:N2}  {1,6:N2}   {2,8:N1}" -f ($dwmCpu + $spikeCpu), ((Avg $gpuDwm) + (Avg $gpuSpike)), (($dwm.WorkingSet64 + $spike.WorkingSet64) / 1MB))
Write-Output ""
Write-Output "simdi spike penceresinde '1'e bas (backdrop kapali) ve bu scripti tekrar calistir."
Write-Output "iki TOPLAM satirinin farki = Acrylic'in gercek maliyeti."
