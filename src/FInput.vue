<template>
  <div
    class="input-div"
    :class="[
      props.class,
      {
        active: isFocus,
      },
    ]"
  >
    <input
      class="input-inner"
      v-model="model"
      v-bind="propsWithoutClass"
      @focus="isFocus = true"
      @blur="isFocus = false"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';
const isFocus = ref(false);
const model = defineModel<string>();
const props = defineProps({
  placeholder: {
    type: String,
    default: '请输入内容',
  },
  disabled: {
    type: Boolean,
    default: false,
  },
  type: {
    type: String,
    default: 'text',
  },
  class: {
    type: String,
    default: '',
  },
});
const propsWithoutClass = computed(() => {
  const { class: _, ...rest } = props;
  return rest;
});
</script>

<style scoped></style>
