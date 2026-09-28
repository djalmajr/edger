{{/*
Expand the name of the chart.
*/}}
{{- define "edger.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "edger.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Immutable image reference. A digest takes precedence over the mutable tag.
*/}}
{{- define "edger.image" -}}
{{- if .Values.image.digest -}}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest -}}
{{- else -}}
{{- printf "%s:%s" .Values.image.repository (default .Chart.AppVersion .Values.image.tag) -}}
{{- end -}}
{{- end }}

{{/*
Common labels.
*/}}
{{- define "edger.labels" -}}
app: {{ include "edger.name" . }}
app.kubernetes.io/name: {{ include "edger.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{- end }}

{{/*
Selector labels.
*/}}
{{- define "edger.selectorLabels" -}}
app: {{ include "edger.name" . }}
app.kubernetes.io/name: {{ include "edger.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Secret name that contains the file-backed root key.
*/}}
{{- define "edger.rootKeySecretName" -}}
{{- if .Values.rootKey.existingSecret -}}
{{- .Values.rootKey.existingSecret -}}
{{- else -}}
{{- printf "%s-root-key" (include "edger.fullname" .) -}}
{{- end -}}
{{- end }}

{{/*
Absolute path passed to EDGER_ROOT_KEY_FILE.
*/}}
{{- define "edger.rootKeyFilePath" -}}
{{- printf "%s/%s" .Values.rootKey.mountPath .Values.rootKey.fileName -}}
{{- end }}

{{/* Existing initial root password Secret, mounted only when configured. */}}
{{- define "edger.rootPasswordFilePath" -}}
{{- "/var/run/secrets/edger-console-root/password" -}}
{{- end }}

{{/*
Directory where the existing Tenancit token Secret is mounted read-only.
Fixed on purpose: the token path is not an operator-facing setting.
*/}}
{{- define "edger.tenancitTokenMountPath" -}}
{{- "/var/run/secrets/edger-tenancit" -}}
{{- end }}

{{/*
Absolute path passed to EDGER_TENANCIT_TOKEN_FILE. The Secret content never
enters the ConfigMap, release values or logs.
*/}}
{{- define "edger.tenancitTokenFilePath" -}}
{{- printf "%s/token" (include "edger.tenancitTokenMountPath" .) -}}
{{- end }}

{{/*
Workers are persisted on one PVC while the routing index remains process-local.
Multiple replicas are therefore unsafe until worker distribution is coordinated.
*/}}
{{- define "edger.validate" -}}
{{- if and .Values.consoleAuth.rootPasswordSecret.name (not .Values.consoleAuth.rootPasswordSecret.key) -}}
{{- fail "consoleAuth.rootPasswordSecret.key é obrigatório quando consoleAuth.rootPasswordSecret.name está configurado" -}}
{{- end -}}
{{- if ne (int .Values.replicaCount) 1 -}}
{{- fail "replicaCount deve ser 1: os workers ficam no PVC, mas o índice de roteamento é mantido em memória por processo; múltiplas réplicas são não determinísticas até existir distribuição/coordenação de workers" -}}
{{- end -}}
{{- if .Values.hpa.enabled -}}
{{- fail "hpa.enabled deve ser false: o autoscaling criaria múltiplas réplicas com índices de workers independentes; habilite HPA somente após implementar distribuição/coordenação de workers" -}}
{{- end -}}
{{- if .Values.tenantRouting.enabled -}}
{{- if not .Values.tenantRouting.tenancit.identifyUrl -}}
{{- fail "tenantRouting.tenancit.identifyUrl é obrigatório com tenantRouting.enabled=true: informe a URL HTTPS exata do endpoint /v1/identify do Tenancit no cluster real" -}}
{{- end -}}
{{- if not .Values.tenantRouting.tenancit.tokenSecret.name -}}
{{- fail "tenantRouting.tenancit.tokenSecret.name é obrigatório com tenantRouting.enabled=true: crie antes o Secret existente com o token do API client tenant:identify" -}}
{{- end -}}
{{- if not .Values.tenantRouting.tenancit.tokenSecret.key -}}
{{- fail "tenantRouting.tenancit.tokenSecret.key é obrigatório com tenantRouting.enabled=true: informe a chave do Secret que contém o token" -}}
{{- end -}}
{{- $tenantUrl := urlParse .Values.tenantRouting.tenancit.identifyUrl -}}
{{- if not $tenantUrl.host -}}
{{- fail "tenantRouting.tenancit.identifyUrl deve incluir um hostname Tenancit válido" -}}
{{- end -}}
{{- $tenantHostname := $tenantUrl.hostname | default $tenantUrl.host -}}
{{- $loopback := or (eq $tenantHostname "localhost") (eq $tenantHostname "127.0.0.1") (eq $tenantHostname "::1") -}}
{{- if and (ne $tenantUrl.scheme "https") (not (and (eq $tenantUrl.scheme "http") $loopback)) -}}
{{- fail "tenantRouting.tenancit.identifyUrl deve ser HTTPS no cluster real (HTTP é aceito apenas em loopback)" -}}
{{- end -}}
{{- /* urlParse.host inclui ":porta"; a porta, quando presente, deve ser numérica. */ -}}
{{- $tenantPort := "" -}}
{{- $bracketed := printf "[%s]" $tenantHostname -}}
{{- if eq $tenantUrl.host $tenantHostname -}}
{{- else if hasPrefix $bracketed $tenantUrl.host -}}
{{- $tenantPort = trimPrefix $bracketed $tenantUrl.host -}}
{{- else if hasPrefix $tenantHostname $tenantUrl.host -}}
{{- $tenantPort = trimPrefix $tenantHostname $tenantUrl.host -}}
{{- else -}}
{{- fail "tenantRouting.tenancit.identifyUrl deve incluir um hostname Tenancit válido" -}}
{{- end -}}
{{- if and $tenantPort (not (regexMatch "^:([0-9]+)?$" $tenantPort)) -}}
{{- fail "tenantRouting.tenancit.identifyUrl tem porta inválida: use host loopback com porta numérica ou sem porta" -}}
{{- end -}}
{{- $tenantPortDigits := trimPrefix ":" $tenantPort -}}
{{- if or (gt (len $tenantPortDigits) 5) (and $tenantPortDigits (gt (atoi $tenantPortDigits) 65535)) -}}
{{- fail "tenantRouting.tenancit.identifyUrl tem porta inválida: use host loopback com porta numérica ou sem porta" -}}
{{- end -}}
{{- if ne $tenantUrl.path "/v1/identify" -}}
{{- fail "tenantRouting.tenancit.identifyUrl deve apontar para o path exato /v1/identify, sem query, fragment ou userinfo" -}}
{{- end -}}
{{- if or $tenantUrl.query $tenantUrl.fragment $tenantUrl.userinfo -}}
{{- fail "tenantRouting.tenancit.identifyUrl deve apontar para o path exato /v1/identify, sem query, fragment ou userinfo" -}}
{{- end -}}
{{- end -}}
{{- end }}
